//! Creates and finalizes a request through the real block lifecycle.
use super::*;

pub(super) fn open_voting() -> VotingOpenScenario {
    open_voting_with_pre_open_state().0
}

/// Committed states in which the job is finalized but its window is not open.
pub(super) struct PreOpenStates {
    /// After block `open_height - 2`: the next block cannot open the window.
    pub(super) two_blocks_before: HashMap<(Address, U256), U256>,
    /// After block `open_height - 1`: the next block's begin zone opens it.
    pub(super) one_block_before: HashMap<(Address, U256), U256>,
}

/// Also returns the committed pre-open states of the same job.
pub(super) fn open_voting_with_pre_open_state() -> (VotingOpenScenario, PreOpenStates) {
    let chain_spec: Arc<ChainSpec<OutbeHeader>> = ChainSpecBuilder::mainnet()
        .reset()
        .paris_activated()
        .build()
        .map_header(OutbeHeader::new)
        .into();
    outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID).unwrap();
    let signer =
        Arc::new(OutbeEvmSigner::from_secret_bytes([1u8; 32]).expect("test proposer key is valid"));
    let proposer = signer.address();
    let dkg = build_dkg();
    let snapshot = build_snapshot(&dkg);
    let genesis_hash = chain_spec.genesis_hash();
    let founder_validators = snapshot
        .committee
        .iter()
        .map(|entry| (entry.address, entry.consensus_pubkey))
        .collect::<Vec<_>>();
    let mut fork_install =
        ForkInstallScenario::measurement_at(PARENT_HEIGHT, CHAIN_ID, genesis_hash)
            .unwrap()
            .with_founder_validators(&founder_validators)
            .unwrap()
            .into_install();
    fork_install
        .request_profile
        .capacity_profile
        .result_deadline_blocks = 8;
    fork_install
        .validate_for_chain(CHAIN_ID, genesis_hash, &poc_schema_limits())
        .expect("short measurement response window remains production-valid");
    let prepared = prepare_parent(&snapshot, genesis_hash, &fork_install);
    let fork_install = Arc::new(fork_install);
    let metadata =
        finalized_parent_metadata(&dkg, &snapshot, PARENT_HEIGHT, prepared.parent.hash());
    let provider = mock_provider(&chain_spec, &prepared.parent_storage);
    provider.inner.add_block(
        prepared.parent.hash(),
        Block::new(prepared.parent.header().clone(), Default::default()),
    );
    assert_provider_snapshot(
        &provider,
        &prepared.parent_storage,
        &snapshot,
        metadata.committee_set_hash,
    );
    assert_provider_activated_ocomp_inputs(&provider, prepared.wwd, prepared.nominal);
    let body_storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
    let runtime_body_readers = RuntimeBodyReaders::new(body_storage);
    let fixture = OcompSuccessorFixture {
        chain_spec: &chain_spec,
        tree_service: &prepared.tree_service,
        signer: &signer,
        runtime_body_readers: &runtime_body_readers,
        fork_install: &fork_install,
        dkg: &dkg,
        snapshot: &snapshot,
    };
    let evm_config = canonical_evm_config(CanonicalEvmConfigInput {
        chain_spec: chain_spec.clone(),
        accounted_parent: provider.inner.clone(),
        runtime_body_readers: runtime_body_readers.clone(),
        signer: signer.clone(),
        tree_service: prepared.tree_service.clone(),
        fork_install: fork_install.clone(),
    });
    let phase1 = evm_config
        .build_signed_phase1_tx(
            REQUEST_HEIGHT,
            CHAIN_ID,
            prepared.parent.hash(),
            Some(metadata.clone()),
            Some(proposer),
        )
        .unwrap()
        .expect("block 2 has a Phase 1 transaction");
    let decoded_metadata = match SystemTxInputV2::decode(phase1.tx().input().as_ref()).unwrap() {
        SystemTxInputV2::CertifiedParentAccounting { metadata } => metadata,
        other => panic!("expected Phase 1 metadata, got {other:?}"),
    };
    assert_eq!(decoded_metadata, metadata);
    outbe_consensus::proof::verify_v2_proof(
        &decoded_metadata,
        &snapshot,
        decoded_metadata.proof.as_ref(),
        prepared.parent.hash(),
    )
    .expect("signed Phase 1 transaction preserves the valid proof");
    let payload_builder = OutbePayloadBuilder::new(
        provider.clone(),
        test_pool(Vec::new()),
        evm_config.clone(),
        EthereumBuilderConfig::new().with_gas_limit(BLOCK_GAS_LIMIT),
    );
    let attributes = OutbePayloadAttributes::new(
        REWARDS_ADDRESS,
        prepared.request_time * 1_000,
        B256::repeat_byte(0x44),
        Some(B256::repeat_byte(0x45)),
        Bytes::new(),
        Some(metadata),
        Some(proposer),
    )
    .with_execution_read_budget(ExecutionReadBudget::new());
    let payload = payload_builder
        .build_empty_payload(PayloadConfig::new(
            prepared.parent.clone(),
            attributes,
            PayloadId::new([0x08; 8]),
        ))
        .expect("production payload builder must create the request block");

    let body = &payload.block().body().transactions;
    let layout = split_system_layout(body).expect("request block has canonical system layout");
    assert_eq!(
        layout
            .begin_block_kinds()
            .expect("begin-zone inputs decode"),
        vec![
            SystemTxKind::CertifiedParentAccounting,
            SystemTxKind::LateFinalizeCredits,
            SystemTxKind::OcompLifecycleBegin,
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::OracleSlashWindow,
            SystemTxKind::HookEvents,
        ]
    );
    assert!(layout.user.is_empty());
    assert_eq!(
        layout.end_block_kinds().expect("end-zone inputs decode"),
        vec![SystemTxKind::OcompTerminalRequest]
    );

    let executed = payload
        .executed_block()
        .expect("builder exposes its exact production execution");
    let receipts = &executed.execution_output.result.receipts;
    assert_eq!(receipts.len(), body.len());
    let cycle_receipt = &receipts[3];
    assert!(cycle_receipt.success);
    let accumulation = cycle_receipt
        .logs
        .iter()
        .find_map(|log| IMetadosis::MetadosisAccumulation::decode_log(log).ok())
        .expect("CycleTick receipt exposes MetadosisAccumulation");
    let expected_cycle_day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(prepared.request_time),
    );
    assert_eq!(accumulation.data.date, expected_cycle_day);
    assert!(
        !accumulation.data.metadosisLimitMinor.is_zero(),
        "production CycleTick must route a non-zero allocation"
    );
    let terminal_receipt = receipts.last().expect("terminal request receipt");
    assert!(terminal_receipt.success);
    let requested = terminal_receipt
        .logs
        .iter()
        .find_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
        .expect("terminal receipt exposes OffchainJobRequested");
    assert_eq!(requested.data.wwd, prepared.wwd.value());
    assert_eq!(requested.data.pendingNonce, 0);
    assert_eq!(requested.data.attempt, 0);

    let replay = evm_config
        .executor(StateProviderDatabase::new(&provider))
        .execute(executed.recovered_block.as_ref())
        .expect("validator replay succeeds through the production executor");
    assert_eq!(
        replay, *executed.execution_output,
        "proposer and validator must agree on receipts, requests and post-state"
    );

    let artifacts = decode_outbe_block_artifacts(payload.block().header().extra_data().as_ref())
        .expect("request header artifacts decode");
    let ce_artifact = artifacts
        .compressed_entities_root
        .expect("request header carries the completed CE seal");
    assert_eq!(
        ce_artifact.commitment_scheme_version,
        ACTIVE_COMMITMENT_SCHEME
    );
    let exact_post_state = provider
        .hashed_post_state(&executed.execution_output.state)
        .unwrap();
    let expected_state_root = provider.state_root_for(exact_post_state.clone());
    assert_eq!(
        payload.block().header().state_root(),
        expected_state_root,
        "header state root must be derived from the real execution BundleState"
    );
    let mutated_state_root = provider.state_root_for(mutate_one_storage_value(exact_post_state));
    assert_ne!(
        mutated_state_root, expected_state_root,
        "the independent root oracle must be sensitive to a real post-state mutation"
    );
    assert_ne!(
        payload.block().header().state_root(),
        prepared.parent.header().state_root(),
        "request execution must produce an observable state-root transition"
    );
    prepared
        .tree_service
        .apply_finalized(REQUEST_HEIGHT, payload.block().hash(), ce_artifact.r_sealed)
        .expect("production CE finalizer applies the exact built candidate");
    assert_eq!(
        prepared.tree_service.finalized_marker().unwrap().new_root,
        ce_artifact.r_sealed
    );

    let mut post_state = HashMapStorageProvider::new(CHAIN_ID);
    post_state.storage = prepared.parent_storage.clone();
    apply_bundle(&mut post_state, executed.execution_output.state.state());
    StorageHandle::enter(&mut post_state, |storage| {
        let update = Update::new(storage.clone());
        assert_eq!(update.get_active_version().unwrap(), ProtocolVersion::ZERO);
        assert_eq!(update.get_active_version_height().unwrap(), 0);
        assert_eq!(
            update.version_at_height(REQUEST_HEIGHT).unwrap(),
            ProtocolVersion::ZERO
        );

        assert!(
            outbe_metadosis::api::is_active_ocomp_fork_install(storage.clone(), &fork_install,)
                .unwrap(),
            "fork block installs the exact complete activation authority"
        );

        let public_call = IMetadosis::getOffchainJobCall {
            intentId: requested.data.intentId,
        };
        let encoded = outbe_metadosis::precompile::dispatch(
            storage.clone(),
            &public_call.abi_encode(),
            proposer,
            U256::ZERO,
        )
        .unwrap();
        let public_bytes = IMetadosis::getOffchainJobCall::abi_decode_returns(&encoded).unwrap();
        let record =
            OcompJobRecordV1::decode_canonical(public_bytes.as_ref(), &poc_schema_limits())
                .unwrap();
        assert_eq!(record.status, OcompJobStatus::AwaitingFinality);
        assert!(
            record.finalized.is_none(),
            "request block cannot invent a response window before finality"
        );
        assert_eq!(record.intent.wwd, prepared.wwd.value());
        assert_eq!(record.intent.pending_nonce, 0);
        assert_eq!(record.intent.authenticated_day_count, 1);
        assert_eq!(record.intent.authenticated_day_nominal, prepared.nominal);
        assert_eq!(record.intent.ce_sealed_root, ce_artifact.r_sealed);
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .metadosis
                .expected_status,
            outbe_ocomp_protocol::intent::MetadosisExpectedStatus::OffchainPending
        );
        let frozen = &record.intent.frozen_metadosis_values;
        let base_limit = outbe_metadosis::api::worldwide_day(storage.clone(), prepared.wwd)
            .unwrap()
            .unwrap()
            .metadosis_limit_minor;
        // The effective ceiling is the day's own emission plus what it drew from the accumulator.
        assert_eq!(
            frozen.day_limit,
            base_limit.checked_add(frozen.desis_limit_minor).unwrap()
        );
        // The auction never asks for more than the nominal beyond the symbolic share.
        assert!(
            frozen.desis_limit_minor
                <= prepared
                    .nominal
                    .checked_sub(frozen.lysis_limit_minor)
                    .expect("Lysis cannot exceed the day's nominal")
        );
        // Lysis took this day's whole emission, so it credited nothing and the auction, with an
        // empty accumulator behind it, had nothing to draw.
        assert_eq!(frozen.lysis_limit_minor, base_limit);
        assert_eq!(frozen.desis_limit_minor, U256::ZERO);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO,
            "nothing was credited and nothing was drawn"
        );
        assert!(!record
            .intent
            .frozen_metadosis_values
            .lysis_limit_minor
            .is_zero());
        assert_ne!(
            record
                .intent
                .frozen_metadosis_values
                .request_limit_split_receipt_hash,
            B256::ZERO
        );
        // The brief waits for the Lysis deadline, so the request leaves Desis untouched.
        assert_eq!(
            DesisContract::new(storage.clone())
                .auction_stage
                .read(&prepared.wwd)
                .unwrap(),
            AuctionStage::None as u8
        );
        assert_eq!(
            DesisContract::new(storage.clone())
                .pending_desis_limit_minor
                .read(&prepared.wwd)
                .unwrap(),
            record.intent.frozen_metadosis_values.desis_limit_minor
        );

        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            IntexContract::new(storage.clone())
                .total_series
                .read()
                .unwrap(),
            0
        );
        assert!(
            outbe_intex::api::certified_contributor_generation(&storage, prepared.wwd)
                .unwrap()
                .is_none()
        );
        let tribute = TributeContract::new(storage);
        assert_eq!(tribute.total_supply().unwrap(), 1);
        let totals = tribute.get_day_totals(prepared.wwd).unwrap();
        assert_eq!(totals.tribute_count, 1);
        assert_eq!(totals.tribute_nominal_total_minor, prepared.nominal);
    });

    let request_hash = payload.block().hash();
    let request_state_root = payload.block().header().state_root();
    let request_parent = Arc::new(SealedHeader::new(
        payload.block().header().clone(),
        request_hash,
    ));
    let successor_provider = mock_provider(&chain_spec, &post_state.storage);
    successor_provider.inner.add_block(
        request_hash,
        Block::new(request_parent.header().clone(), Default::default()),
    );
    let successor_metadata =
        finalized_parent_metadata(&dkg, &snapshot, REQUEST_HEIGHT, request_hash);
    let successor_builder = OutbePayloadBuilder::new(
        successor_provider.clone(),
        test_pool(Vec::new()),
        evm_config.clone(),
        EthereumBuilderConfig::new().with_gas_limit(BLOCK_GAS_LIMIT),
    );
    let successor_attributes = OutbePayloadAttributes::new(
        REWARDS_ADDRESS,
        (prepared.request_time + 1) * 1_000,
        B256::repeat_byte(0x54),
        Some(B256::repeat_byte(0x55)),
        Bytes::new(),
        Some(successor_metadata),
        Some(proposer),
    )
    .with_execution_read_budget(ExecutionReadBudget::new());
    let successor = successor_builder
        .build_empty_payload(PayloadConfig::new(
            request_parent,
            successor_attributes,
            PayloadId::new([0x09; 8]),
        ))
        .expect("the certified successor records request finality");
    let successor_execution = successor
        .executed_block()
        .expect("successor exposes its production execution");
    let mut finalized_state = HashMapStorageProvider::new(CHAIN_ID);
    finalized_state.storage = post_state.storage;
    apply_bundle(
        &mut finalized_state,
        successor_execution.execution_output.state.state(),
    );
    StorageHandle::enter(&mut finalized_state, |storage| {
        let record = OcompJobRecordV1::decode_canonical(
            &outbe_metadosis::api::get_offchain_job(storage, requested.data.intentId).unwrap(),
            &poc_schema_limits(),
        )
        .expect("certified successor preserves the request record");
        let finalized = record
            .finalized
            .expect("actual parent finalization must bind the request on-chain");
        assert_eq!(record.status, OcompJobStatus::AwaitingFinality);
        assert_eq!(finalized.finalized_request_block_hash, request_hash);
        assert_eq!(finalized.finalized_request_state_root, request_state_root);
        assert_eq!(finalized.finality_recorded_height, REQUEST_HEIGHT + 1);
        assert_eq!(finalized.open_height, REQUEST_HEIGHT + 5);
        assert_eq!(
            finalized.job_id,
            record
                .intent
                .job_id(request_hash, request_state_root, &poc_schema_limits())
                .unwrap()
        );
    });

    let finalized_record = StorageHandle::enter(&mut finalized_state, |storage| {
        let encoded =
            outbe_metadosis::api::get_offchain_job(storage, requested.data.intentId).unwrap();
        OcompJobRecordV1::decode_canonical(&encoded, &poc_schema_limits()).unwrap()
    });
    let finalized = finalized_record.finalized.as_ref().unwrap();
    let open_height = finalized.open_height;
    let successor_artifacts =
        decode_outbe_block_artifacts(successor.block().header().extra_data().as_ref()).unwrap();
    let successor_ce = successor_artifacts
        .compressed_entities_root
        .expect("certified successor carries its completed CE seal");
    prepared
        .tree_service
        .apply_finalized(
            REQUEST_HEIGHT + 1,
            successor.block().hash(),
            successor_ce.r_sealed,
        )
        .expect("certified successor CE candidate finalizes before its child");

    let mut canonical_parent = Arc::new(SealedHeader::new(
        successor.block().header().clone(),
        successor.block().hash(),
    ));
    let mut canonical_storage = finalized_state.storage;
    let mut two_blocks_before = None;
    for height in (REQUEST_HEIGHT + 2)..open_height {
        let built = build_canonical_ocomp_successor(
            fixture,
            OcompSuccessorBlock {
                proposer,
                parent: canonical_parent,
                parent_storage: &canonical_storage,
                height,
                timestamp: prepared.request_time + (height - REQUEST_HEIGHT),
                intent_id: requested.data.intentId,
                user_transactions: Vec::new(),
            },
        );
        assert_eq!(
            built.record.status,
            OcompJobStatus::AwaitingFinality,
            "pre-open production lifecycle must preserve AwaitingFinality"
        );
        assert!(built.requested_intents.is_empty());
        canonical_parent = built.header;
        canonical_storage = built.storage;
        if height + 2 == open_height {
            two_blocks_before = Some(canonical_storage.clone());
        }
    }

    let pre_open_states = PreOpenStates {
        two_blocks_before: two_blocks_before
            .expect("the lifecycle builds the block two heights before its window opens"),
        one_block_before: canonical_storage.clone(),
    };
    let voting_open = build_canonical_ocomp_successor(
        fixture,
        OcompSuccessorBlock {
            proposer,
            parent: canonical_parent,
            parent_storage: &canonical_storage,
            height: open_height,
            timestamp: prepared.request_time + (open_height - REQUEST_HEIGHT),
            intent_id: requested.data.intentId,
            user_transactions: Vec::new(),
        },
    );
    assert_eq!(voting_open.record.status, OcompJobStatus::VotingOpen);

    assert_ne!(requested.data.intentId, B256::ZERO);
    assert_eq!(
        terminal_receipt
            .logs
            .iter()
            .filter(|log| {
                log.address == METADOSIS_ADDRESS
                    && IMetadosis::OffchainJobRequested::decode_log(log).is_ok()
            })
            .count(),
        1
    );
    assert!(terminal_receipt
        .logs
        .iter()
        .all(|log| log.address != TRIBUTE_FACTORY_ADDRESS));

    (
        VotingOpenScenario {
            chain_spec,
            prepared,
            signer,
            runtime_body_readers,
            fork_install,
            dkg,
            snapshot,
            proposer,
            open_height,
            intent_id: requested.data.intentId,
            finalized_record,
            voting_open,
        },
        pre_open_states,
    )
}
