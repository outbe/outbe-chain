use super::*;

const ACTIVE_COUNT: usize = 128;
const DEADLINE: u64 = 2;

fn prepare_expiring_committee(
    proposer: Address,
) -> (State<CacheDB<EmptyDBTyped<ProviderError>>>, Vec<Address>) {
    let mut validators = Vec::with_capacity(ACTIVE_COUNT);
    validators.push((proposer, dummy_pubkey(0x80)));
    for index in 1..ACTIVE_COUNT {
        validators.push((
            numbered_test_address(0x81, index as u64),
            dummy_pubkey(index as u8),
        ));
    }
    let addresses: Vec<_> = validators.iter().map(|(address, _)| *address).collect();
    let state = state_with_active_validators_seeded_at_block(&validators, 1, |storage| {
        seed_expiring_tee_nodes(storage, &addresses, DEADLINE)
            .expect("seed expiring tee nodes fixture succeeds");
    });

    (state, addresses)
}

fn expiry_context(
    parent_hash: B256,
    parent_metadata: CertifiedParentAccountingMetadata,
    proposer: Address,
) -> OutbeBlockExecutionCtx<'static> {
    fixtures::parent_accounting_context(fixtures::ParentAccountingFixture {
        parent_hash,
        metadata: parent_metadata,
        artifact: AccountedParentArtifact {
            summary: ExecutionSummaryArtifact {
                validator_fee_sum: U256::ZERO,
            },
            timestamp: 1,
            state_root: Some(B256::repeat_byte(0x92)),
        },
        proposer,
    })
}

fn assert_expiry_budget(receipt: &Receipt, cycle_gas: u64, cycle_internal_gas: u64) {
    assert!(receipt.success, "worst-case TEE expiry sweep must not OOG");
    assert_eq!(
        receipt
            .logs
            .iter()
            .filter(|log| {
                log.address == outbe_primitives::addresses::VALIDATOR_SET_ADDRESS
                    && log.data.topics().first()
                        == Some(&keccak256("ValidatorJailed(address,uint64)"))
            })
            .count(),
        ACTIVE_COUNT
    );
    eprintln!(
        "TEE expiry CycleTick gas: active={ACTIVE_COUNT}, visible={cycle_gas}, internal={cycle_internal_gas}, limit=30000000"
    );
    assert!(cycle_gas < 30_000_000);
    assert!(cycle_internal_gas < 30_000_000);
}

pub(super) fn run() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let (mut state, addresses) = prepare_expiring_committee(proposer);
    let mut evm_env = test_evm_env(2, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(DEADLINE);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let parent_hash = B256::repeat_byte(0x91);
    let mut parent_metadata = metadata_with(addresses.clone(), vec![1; ACTIVE_COUNT], Vec::new());
    parent_metadata.finalized_block_number = 1;
    parent_metadata.finalized_block_hash = parent_hash;
    let evm = config.evm_with_env(&mut state, evm_env);
    let execution = expiry_context(parent_hash, parent_metadata.clone(), proposer);
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply");
    });

    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(parent_metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let cycle =
        fixtures::observe_cycle_tick(system_txs, "valid begin-zone system transaction", |tx| {
            let internal_before = executor.system_tx_execution_gas;
            let output = executor
                .execute_transaction(tx)
                .expect("TEE expiry begin-zone prefix should execute");
            fixtures::CycleObservation {
                visible_gas: output.tx_gas_used(),
                internal_gas: executor
                    .system_tx_execution_gas
                    .saturating_sub(internal_before),
                receipt_index: executor.receipts().len() - 1,
            }
        });
    let cycle_gas = cycle
        .as_ref()
        .map(|cycle| cycle.visible_gas)
        .expect("CycleTick gas must be captured");
    let cycle_internal_gas = cycle
        .as_ref()
        .map(|cycle| cycle.internal_gas)
        .expect("CycleTick internal gas must be captured");
    let receipt = &executor.receipts()[cycle
        .map(|cycle| cycle.receipt_index)
        .expect("CycleTick receipt index")];

    assert_expiry_budget(receipt, cycle_gas, cycle_internal_gas);
}
