use super::super::*;

pub(in crate::lifecycle) struct CanonicalOcompSuccessor {
    pub(in crate::lifecycle) header: Arc<SealedHeader<OutbeHeader>>,
    pub(in crate::lifecycle) storage: HashMap<(Address, U256), U256>,
    pub(in crate::lifecycle) record: OcompJobRecordV1,
    pub(in crate::lifecycle) requested_intents: Vec<B256>,
    pub(in crate::lifecycle) user_transaction_count: usize,
    pub(in crate::lifecycle) user_transaction_hashes: Vec<B256>,
    pub(in crate::lifecycle) user_receipt_successes: Vec<bool>,
    pub(in crate::lifecycle) user_receipt_cumulative_gas: Vec<u64>,
    pub(in crate::lifecycle) header_state_root: B256,
}

pub(in crate::lifecycle) fn canonical_evm_config(
    chain_spec: Arc<ChainSpec<OutbeHeader>>,
    accounted_parent: InnerTestProvider,
    runtime_body_readers: RuntimeBodyReaders,
    signer: Arc<OutbeEvmSigner>,
    tree_service: Arc<CompressedTreeService>,
    fork_install: Arc<outbe_metadosis::config::OcompForkInstallV1>,
) -> OutbeEvmConfig {
    OutbeEvmConfig::new_with_provider_and_runtime_body_readers(
        chain_spec,
        Arc::new(RethAccountedParentArtifactProvider::new(
            accounted_parent,
            None,
        )),
        runtime_body_readers,
    )
    .with_evm_signer(signer)
    .with_compressed_tree_service(tree_service)
    .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(PARENT_HEIGHT))
    .with_ocomp_fork_install(fork_install)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::lifecycle) fn build_canonical_ocomp_successor(
    chain_spec: &Arc<ChainSpec<OutbeHeader>>,
    tree_service: &Arc<CompressedTreeService>,
    signer: &Arc<OutbeEvmSigner>,
    runtime_body_readers: &RuntimeBodyReaders,
    fork_install: &Arc<outbe_metadosis::config::OcompForkInstallV1>,
    dkg: &Dkg,
    snapshot: &CommitteeSnapshot,
    proposer: Address,
    parent: Arc<SealedHeader<OutbeHeader>>,
    parent_storage: &HashMap<(Address, U256), U256>,
    height: u64,
    timestamp: u64,
    intent_id: B256,
    user_transactions: Vec<EthPooledTransaction>,
) -> CanonicalOcompSuccessor {
    assert_eq!(height, parent.number() + 1);
    let provider = mock_provider(chain_spec, parent_storage);
    provider.inner.add_block(
        parent.hash(),
        Block::new(parent.header().clone(), Default::default()),
    );
    let evm_config = canonical_evm_config(
        chain_spec.clone(),
        provider.inner.clone(),
        runtime_body_readers.clone(),
        signer.clone(),
        tree_service.clone(),
        fork_install.clone(),
    );
    let metadata = finalized_parent_metadata(dkg, snapshot, height - 1, parent.hash());
    let user_transaction_count = user_transactions.len();
    let transaction_pool = test_pool(user_transactions);
    let pool_size = transaction_pool.pool_size();
    assert_eq!(
        pool_size.total, user_transaction_count,
        "canonical OCOMP model pool must retain every supplied public vote: {pool_size:?}"
    );
    assert_eq!(
        pool_size.pending, user_transaction_count,
        "canonical OCOMP model pool must make every supplied public vote executable: {pool_size:?}"
    );
    let builder = OutbePayloadBuilder::new(
        provider.clone(),
        transaction_pool,
        evm_config.clone(),
        EthereumBuilderConfig::new().with_gas_limit(BLOCK_GAS_LIMIT),
    );
    let attributes = OutbePayloadAttributes::new(
        REWARDS_ADDRESS,
        timestamp * 1_000,
        B256::from(U256::from(height).to_be_bytes::<32>()),
        Some(B256::from(
            U256::from(height.saturating_add(1)).to_be_bytes::<32>(),
        )),
        Bytes::new(),
        Some(metadata),
        Some(proposer),
    )
    .with_execution_read_budget(ExecutionReadBudget::new());
    let payload_config = PayloadConfig::new(
        parent,
        attributes,
        PayloadId::new([u8::try_from(height).unwrap_or(u8::MAX); 8]),
    );
    let payload = if user_transaction_count == 0 {
        builder
            .build_empty_payload(payload_config)
            .expect("canonical OCOMP model successor builds")
    } else {
        builder
            .try_build(BuildArguments::new(
                Default::default(),
                Default::default(),
                None,
                payload_config,
                Default::default(),
                None,
            ))
            .expect("canonical OCOMP model successor with public votes builds")
            .into_payload()
            .expect("canonical OCOMP model public-vote build returns a payload")
    };
    let layout = split_system_layout(&payload.block().body().transactions)
        .expect("canonical OCOMP model block has a system layout");
    assert!(layout
        .begin_block_kinds()
        .expect("canonical OCOMP model begin zone decodes")
        .contains(&SystemTxKind::OcompLifecycleBegin));
    assert!(
        layout.user.len() <= user_transaction_count,
        "payload cannot include more public transactions than the supplied pool"
    );
    let executed = payload
        .executed_block()
        .expect("canonical OCOMP model block exposes execution");
    let (user_transaction_hashes, user_receipt_successes, user_receipt_cumulative_gas) =
        if !layout.user.is_empty() {
            let user_receipts = &executed.execution_output.result.receipts
                [layout.begin.len()..layout.begin.len() + layout.user.len()];
            let mut prior_cumulative_gas = executed.execution_output.result.receipts
                [layout.begin.len().saturating_sub(1)]
            .cumulative_gas_used;
            for (transaction, receipt) in layout.user.iter().zip(user_receipts) {
                let transaction = *transaction;
                if transaction.to() == Some(METADOSIS_ADDRESS) {
                    assert_eq!(
                        TransactionSigned::gas_limit(transaction),
                        30_000,
                        "every OCOMP system carrier preserves the canonical signed gas limit"
                    );
                    assert_eq!(
                        receipt.cumulative_gas_used, prior_cumulative_gas,
                        "OCOMP system carrier must not consume ordinary user-lane gas"
                    );
                }
                prior_cumulative_gas = receipt.cumulative_gas_used;
            }
            (
                layout
                    .user
                    .iter()
                    .map(|transaction| *(*transaction).tx_hash())
                    .collect(),
                user_receipts
                    .iter()
                    .map(|receipt| receipt.success)
                    .collect(),
                user_receipts
                    .iter()
                    .map(|receipt| receipt.cumulative_gas_used)
                    .collect(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
    let import = evm_config
        .executor(StateProviderDatabase::new(&provider))
        .execute(executed.recovered_block.as_ref())
        .expect("canonical OCOMP model block imports through the production executor");
    assert_eq!(
        import, *executed.execution_output,
        "proposer/import must agree at OCOMP model height {height}"
    );
    let historical_replay = evm_config
        .executor(StateProviderDatabase::new(&provider))
        .execute(executed.recovered_block.as_ref())
        .expect("canonical OCOMP model block replays from historical parent state");
    assert_eq!(
        historical_replay, *executed.execution_output,
        "proposer/historical replay must agree at OCOMP model height {height}"
    );
    let exact_post_state = provider
        .hashed_post_state(&executed.execution_output.state)
        .unwrap();
    let header_state_root = payload.block().header().state_root();
    assert_eq!(
        header_state_root,
        provider.state_root_for(exact_post_state),
        "canonical OCOMP block state root must match its exact production state"
    );

    let mut state = HashMapStorageProvider::new(CHAIN_ID);
    state.storage = parent_storage.clone();
    apply_bundle(&mut state, executed.execution_output.state.state());
    let record = StorageHandle::enter(&mut state, |storage| {
        let encoded = outbe_metadosis::api::get_offchain_job(storage, intent_id)
            .expect("canonical public OCOMP job query");
        OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits())
            .expect("canonical public OCOMP job decodes")
    });
    let requested_intents = executed
        .execution_output
        .result
        .receipts
        .iter()
        .flat_map(|receipt| &receipt.logs)
        .filter_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
        .map(|event| event.data.intentId)
        .collect::<Vec<_>>();
    let artifacts =
        decode_outbe_block_artifacts(payload.block().header().extra_data().as_ref()).unwrap();
    let ce = artifacts
        .compressed_entities_root
        .expect("canonical OCOMP model block carries its completed CE seal");
    tree_service
        .apply_finalized(height, payload.block().hash(), ce.r_sealed)
        .expect("canonical OCOMP model CE candidate finalizes before its child");
    CanonicalOcompSuccessor {
        header: Arc::new(SealedHeader::new(
            payload.block().header().clone(),
            payload.block().hash(),
        )),
        storage: state.storage,
        record,
        requested_intents,
        user_transaction_count: layout.user.len(),
        user_transaction_hashes,
        user_receipt_successes,
        user_receipt_cumulative_gas,
        header_state_root,
    }
}
