use super::*;
use outbe_offchain_data::runtime_body_readers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedWrite {
    EthereumPostBlock,
    CompressedEntitiesSeal,
    TerminalTransaction,
}

fn prepare_terminal_block() -> (
    State<CacheDB<EmptyDBTyped<ProviderError>>>,
    OutbeEvmConfig,
    Address,
) {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let state = state_with_active_proposer_without_ocomp(proposer);
    let chain_spec = test_chain_spec();
    let install = test_ocomp_fork_install(&chain_spec, &[(proposer, dummy_pubkey(0xA2))]);
    let config = OutbeEvmConfig::new_with_runtime_body_readers(
        chain_spec,
        runtime_body_readers(Arc::new(MemoryStorage::new())),
    )
    .with_evm_signer(signer)
    .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(1))
    .with_ocomp_fork_install(install);

    (state, config, proposer)
}

fn terminal_context(
    tee_bootstrap: outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2,
) -> OutbeBlockExecutionCtx<'static> {
    let mut ctx = execution_ctx_with_tee_bootstrap(Some(5), Bytes::new(), tee_bootstrap);
    ctx.inner.withdrawals = Some(std::borrow::Cow::Owned(Vec::new()));

    ctx
}

fn classify_write(changes: revm::state::EvmState) -> Option<ObservedWrite> {
    // Revm reports committed state directly. Distinguish the terminal
    // call target, CE-only seal, and Ethereum post-block commits.
    if changes.contains_key(&outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS) {
        Some(ObservedWrite::TerminalTransaction)
    } else if changes.len() == 1
        && changes.contains_key(&outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS)
    {
        Some(ObservedWrite::CompressedEntitiesSeal)
    } else {
        Some(ObservedWrite::EthereumPostBlock)
    }
}

fn assert_begin_phases(begin: &[Recovered<TransactionSigned>]) {
    assert_eq!(
        begin
            .iter()
            .map(|tx| SystemTxInputV2::decode(tx.tx().input().as_ref())
                .unwrap()
                .kind())
            .collect::<Vec<_>>(),
        vec![
            SystemTxKind::OcompLifecycleBegin,
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::TeeBootstrap,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ]
    );
}

fn assert_terminal_write_order(writes_after_terminal: &[ObservedWrite]) {
    let ethereum_post_block_index = writes_after_terminal
        .iter()
        .position(|write| *write == ObservedWrite::EthereumPostBlock)
        .expect("standard Ethereum post-block changes execute before OSR2");
    let compressed_entities_seal_index = writes_after_terminal
        .iter()
        .position(|write| *write == ObservedWrite::CompressedEntitiesSeal)
        .expect("compressed entities seal executes after OSR2");
    let terminal_transaction_index = writes_after_terminal
        .iter()
        .position(|write| *write == ObservedWrite::TerminalTransaction)
        .expect("OSR2 commits as the terminal transaction");
    assert!(
        ethereum_post_block_index < terminal_transaction_index
            && terminal_transaction_index < compressed_entities_seal_index,
        "semantic write order must be Ethereum post-block -> OSR2 -> final CE seal; got \
         {writes_after_terminal:?}"
    );
}

pub(super) fn run() {
    let (mut state, config, proposer) = prepare_terminal_block();
    let block_timestamp = 1_700_000_000u64;
    let tee_bootstrap = sample_tee_bootstrap_payload_at(1, block_timestamp);
    let mut evm_env = test_evm_env(1, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(block_timestamp);
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = terminal_context(tee_bootstrap.clone());
    let mut executor = config.create_executor(evm, ctx);

    let observed_writes = Arc::new(Mutex::new(Vec::new()));
    executor
        .evm_mut()
        .db_mut()
        .set_state_hook(Some(Box::new(write_observer(observed_writes.clone()))));

    executor
        .apply_pre_execution_changes()
        .expect("active block pre-execution succeeds");
    let begin = prepare_begin(&config, proposer, tee_bootstrap);
    assert_begin_phases(&begin);
    for tx in begin.iter().cloned() {
        executor
            .execute_transaction(tx)
            .expect("begin system tx executes");
    }

    observed_writes.lock().unwrap().clear();
    let end = config
        .build_end_system_txs(1, CHAIN_ID, begin.len(), Some(proposer))
        .expect("terminal system tx builds");
    assert_eq!(end.len(), 1);
    executor
        .execute_transaction(end.into_iter().next().unwrap())
        .expect("terminal system tx executes before the final CE seal");

    let writes_after_terminal = observed_writes.lock().unwrap().clone();
    assert_terminal_write_order(&writes_after_terminal);
    assert!(executor.compressed_entities_seal_output().is_some());
    let receipt_count = executor.receipts().len();
    let later_user = test_regular_tx()
        .try_into_recovered()
        .expect("regular tx signer recovers");
    assert!(executor.execute_transaction(later_user).is_err());
    assert_eq!(executor.receipts().len(), receipt_count);
    assert_eq!(
        observed_writes
            .lock()
            .unwrap()
            .iter()
            .filter(|write| **write == ObservedWrite::CompressedEntitiesSeal)
            .count(),
        1
    );

    executor
        .prepare_final_header_artifacts(0)
        .expect("sealed CE root enters final header");
    let writes_before_finish = observed_writes.lock().unwrap().clone();
    let (_evm, result) = executor.finish().expect("active executor finishes");
    assert_eq!(result.receipts.len(), 7);
    assert_eq!(
        *observed_writes.lock().unwrap(),
        writes_before_finish,
        "finish must not perform any semantic write after OSR2"
    );
}

fn prepare_begin(
    config: &OutbeEvmConfig,
    proposer: Address,
    tee_bootstrap: outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2,
) -> Vec<Recovered<TransactionSigned>> {
    begin_system_txs_for_test(
        config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::explicit(Some(tee_bootstrap)),
        },
    )
}

fn write_observer(
    hook_writes: Arc<Mutex<Vec<ObservedWrite>>>,
) -> impl Fn(revm::state::EvmState) + Send + Sync + 'static {
    move |changes| {
        if let Some(observed) = classify_write(changes) {
            hook_writes.lock().unwrap().push(observed);
        }
    }
}
