//! System receipts that publish hook events: slashing, update activation and factory proposals.

use super::*;

#[test]
fn apply_pre_execution_changes_emits_phase1_slashing_logs_in_system_receipt() {
    let signer = test_evm_signer();
    let proposer = signer.address();
    let absent = address!("0x2222222222222222222222222222222222222222");
    let parent_hash = B256::with_last_byte(0xAA);
    let mut state = state_with_active_validators_seeded(
        &[(proposer, dummy_pubkey(0xA2)), (absent, dummy_pubkey(0xB3))],
        |storage| {
            let si = outbe_slashindicator::contract::SlashIndicator::new(storage);
            si.config_voter_misdemeanor_threshold.write(1).unwrap();
            si.config_proposer_felony_threshold.write(1).unwrap();
        },
    );
    let mut metadata = test_metadata();
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = parent_hash;
    metadata.ordered_committee = vec![proposer, absent];
    metadata.signer_bitmap = vec![1, 0];
    metadata.missed_proposers = vec![outbe_primitives::consensus_metadata::MissedProposerEvent {
        view: 0,
        validator: absent,
    }];

    let bridge = ConsensusExecutionBridge::new();
    bridge.record_execution_summary_with_state_root(
        1,
        parent_hash,
        ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        1,
        B256::repeat_byte(0x91),
    );
    let config =
        OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge).with_evm_signer(signer.clone());
    let evm_env = test_evm_env(2, REWARDS_ADDRESS);
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(0), Bytes::new());
    ctx.inner.parent_hash = parent_hash;
    ctx.parent_consensus_metadata = Some(metadata.clone());
    let mut executor = config.create_executor(evm, ctx);

    // Disable the Phase 1 `verify_v2_proof` preflight. This unit test checks the path that
    // emits slashing logs, not the verifier. The test does not seed a matching committee
    // snapshot.
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("pre-execution changes should apply before Phase 1 system tx");
    });
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    for tx in system_txs {
        executor
            .execute_transaction(tx)
            .expect("Phase 1 slashing system tx should execute");
    }

    // CPA(0) + LateFinalizeCredits(1) + CycleTick(2) + RewardsGemDelivery(3)
    // + OracleSlashWindow(4) + HookEvents(5).
    assert_eq!(executor.receipts().len(), 6);
    let phase1_logs = &executor.receipts()[0].logs;
    let voter_misdemeanor = keccak256("VoterMisdemeanor(address,uint64)");
    let voter_felony = keccak256("VoterFelony(address,uint64,uint64)");
    let proposer_felony = keccak256("ProposerFelony(address,uint64,uint64)");
    // Voter miss / slashing accounting no longer runs in Phase 1 (CPA). It runs at the
    // inclusion-window close at N+K. Thus CPA emits no voter slashing log.
    assert!(
        !phase1_logs.iter().any(|log| {
            log.address == SLASH_INDICATOR_ADDRESS
                && matches!(
                    log.data.topics().first(),
                    Some(topic) if *topic == voter_misdemeanor || *topic == voter_felony
                )
        }),
        "Phase 1 (CPA) must no longer emit voter slashing - it is relocated to window close"
    );
    // Proposer slashing stays in Phase 1 (driven by `missed_proposers` metadata).
    assert!(
        phase1_logs.iter().any(|log| {
            log.address == SLASH_INDICATOR_ADDRESS
                && log.data.topics().first() == Some(&proposer_felony)
        }),
        "Phase 1 proposer slashing must emit receipt-visible ProposerFelony"
    );
    drop(executor);

    let read_ctx = BlockContext::new(2, 2, CHAIN_ID, proposer, vec![proposer, absent]);
    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let si = outbe_slashindicator::contract::SlashIndicator::new(storage.clone());
        // The inclusion-window close (N+K) now counts the voter miss, not CPA. Block 2's CPA
        // leaves voter_miss_count untouched.
        assert_eq!(si.voter_miss_count.read(&absent)?, 0);
        // Proposer slashing stays at CPA. The missed proposer becomes JAILED (felony
        // threshold 1), and CPA records its proposer miss.
        assert_eq!(si.proposer_miss_count.read(&absent)?, 1);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let record = vs.get_validator(absent)?.expect("absent validator exists");
        assert_eq!(record.status, outbe_validatorset::logic::status::JAILED);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("slashing state should be readable");
}

#[test]
fn pre_exec_hooks_emit_whitelisted_update_activation_event() {
    use alloy_sol_types::SolEvent;
    use outbe_update::payload::encode_schedule_update_json;
    use outbe_update::precompile::IUpdate;
    use serde_json::Value;

    let proposer = test_evm_signer().address();
    const ACTIVATION_BLOCK: u64 = 101;
    let protocol_version = outbe_update::constants::PROTOCOL_VERSION;

    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let proposal_id = U256::from(1);
            let payload: Value = serde_json::from_str(&encode_schedule_update_json(
                protocol_version,
                ACTIVATION_BLOCK,
                "",
            ))
            .expect("schedule update JSON should parse");
            let mut update = outbe_update::schema::Update::new(storage.clone());
            update
                .schedule_update_from_propose(proposal_id, &payload, 1)
                .expect("schedule update");
        });

    let ctx = BlockContext::new(
        ACTIVATION_BLOCK,
        ACTIVATION_BLOCK,
        CHAIN_ID,
        proposer,
        vec![proposer],
    );
    let (_, hook_events) = super::run_atomic_storage_hooks(&mut state, ctx, |hook_ctx| {
        super::run_outbe_pre_execution_hooks(hook_ctx, None)
    })
    .expect("pre-exec hooks should run");
    let (whitelisted, _) = partition_hook_events(&hook_events);
    let upgrade_activated = IUpdate::UpgradeActivated::SIGNATURE_HASH;
    assert!(
        whitelisted.iter().any(|log| {
            log.address == UPDATE_ADDRESS && log.data.topics().first() == Some(&upgrade_activated)
        }),
        "pre-exec hooks must emit whitelisted UpgradeActivated for HookEvents receipt"
    );
}

#[test]
fn hook_events_receipt_carries_whitelisted_update_activation_log() {
    use alloy_sol_types::SolEvent;
    use outbe_update::payload::encode_schedule_update_json;
    use outbe_update::precompile::IUpdate;
    use serde_json::Value;

    let proposer = test_evm_signer().address();
    const ACTIVATION_BLOCK: u64 = 101;
    let protocol_version = outbe_update::constants::PROTOCOL_VERSION;

    let mut state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
            let proposal_id = U256::from(1);
            let payload: Value = serde_json::from_str(&encode_schedule_update_json(
                protocol_version,
                ACTIVATION_BLOCK,
                "",
            ))
            .expect("schedule update JSON should parse");
            let mut update = outbe_update::schema::Update::new(storage.clone());
            update
                .schedule_update_from_propose(proposal_id, &payload, 1)
                .expect("schedule update");
        });

    let ctx = BlockContext::new(
        ACTIVATION_BLOCK,
        ACTIVATION_BLOCK,
        CHAIN_ID,
        proposer,
        vec![proposer],
    );
    let (_, hook_events) = super::run_atomic_storage_hooks(&mut state, ctx, |hook_ctx| {
        super::run_outbe_pre_execution_hooks(hook_ctx, None)
    })
    .expect("pre-exec hooks should emit activation events");
    let (whitelisted_logs, _) = partition_hook_events(&hook_events);

    let config = OutbeEvmConfig::new(test_chain_spec());
    let evm = config.evm_with_env(&mut state, test_evm_env(ACTIVATION_BLOCK, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
    executor
        .push_hook_events_receipt(alloy_consensus::TxType::Legacy, whitelisted_logs, 21_000)
        .expect("HookEvents receipt should publish captured hook logs");

    let hook_receipt = executor.receipts().last().expect("HookEvents receipt");
    assert!(hook_receipt.success);
    let upgrade_activated = IUpdate::UpgradeActivated::SIGNATURE_HASH;
    assert!(
        hook_receipt.logs.iter().any(|log| {
            log.address == UPDATE_ADDRESS && log.data.topics().first() == Some(&upgrade_activated)
        }),
        "HookEvents receipt must carry UpgradeActivated from pre-exec hook events"
    );
}

#[test]
fn real_factory_approval_is_published_in_hook_events_receipt() {
    factory_approval::run();
}

#[test]
fn real_factory_execution_error_has_no_factory_receipt_log() {
    const CREATION_BLOCK: u64 = 7;
    let issuer = Address::repeat_byte(0x11);
    let validators = [
        (Address::repeat_byte(0xb1), dummy_pubkey(0xb1)),
        (Address::repeat_byte(0xb2), dummy_pubkey(0xb2)),
        (Address::repeat_byte(0xb3), dummy_pubkey(0xb3)),
    ];
    let payload = encode_canonical_stablecoin_create(&StablecoinCreatePayload {
        issuer,
        name: "Example Dollar".into(),
        ticker: "EXUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    })
    .expect("canonical Factory payload");
    let payload = core::str::from_utf8(&payload).expect("canonical payload is UTF-8");
    let mut state =
        state_with_active_validators_seeded_at_block(&validators, CREATION_BLOCK, |storage| {
            storage
                .set_balance(VOTE_ADDRESS, STABLECOIN_CREATE_BOND)
                .unwrap();
            let mut vote = Vote::new(storage);
            let proposal_id = vote
                .create_proposal_with_value(
                    outbe_vote::ProposalSubmission {
                        proposer: issuer,
                        target_module: STABLECOIN_FACTORY_ADDRESS,
                        payload,
                        created_height: CREATION_BLOCK,
                        attached_value: STABLECOIN_CREATE_BOND,
                    },
                    crate::handlers::vote::registry(),
                )
                .unwrap();
            let mut corrupted = vote.proposals.get(proposal_id).unwrap().unwrap();
            corrupted.payload = "{".into();
            vote.proposals.update(&corrupted).unwrap();
            vote.cast_vote_approve(proposal_id, validators[0].0, true, CREATION_BLOCK + 1)
                .unwrap();
            vote.cast_vote_approve(proposal_id, validators[1].0, true, CREATION_BLOCK + 1)
                .unwrap();
        });

    let finalization_block = CREATION_BLOCK + VOTING_WINDOW_BLOCKS + 1;
    let block_context = BlockContext::new(
        finalization_block,
        1_700_000_000,
        CHAIN_ID,
        issuer,
        validators.iter().map(|(address, _)| *address).collect(),
    );
    let (_, hook_events) =
        super::run_atomic_storage_hooks(&mut state, block_context.clone(), |hook_ctx| {
            super::run_outbe_pre_execution_hooks(hook_ctx, None)
        })
        .expect("typed target Error must not fail the outer hook batch");
    let (receipt_logs, _) = partition_hook_events(&hook_events);
    assert!(receipt_logs.iter().all(|log| {
        log.address != STABLECOIN_FACTORY_ADDRESS
            || log.data.topics().first()
                != Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
    }));
    // A target Error refunds the bond once and burns nothing.
    assert_eq!(
        receipt_logs
            .iter()
            .filter(|log| {
                log.address == VOTE_ADDRESS
                    && log.data.topics().first()
                        == Some(&IVote::ProposalBondRefunded::SIGNATURE_HASH)
            })
            .count(),
        1
    );
    assert!(receipt_logs.iter().all(|log| {
        log.address != VOTE_ADDRESS
            || log.data.topics().first() != Some(&IVote::ProposalBondBurned::SIGNATURE_HASH)
    }));
    {
        let config = OutbeEvmConfig::new(test_chain_spec());
        let evm = config.evm_with_env(
            &mut state,
            test_evm_env(finalization_block, REWARDS_ADDRESS),
        );
        let mut executor = config.create_executor(evm, execution_ctx(None, Bytes::new()));
        executor
            .push_hook_events_receipt(alloy_consensus::TxType::Legacy, receipt_logs, 21_000)
            .expect("Error outcome HookEvents receipt");
        let receipt = executor.receipts().last().expect("HookEvents receipt");
        assert!(receipt.logs.iter().all(|log| {
            log.address != STABLECOIN_FACTORY_ADDRESS
                || log.data.topics().first()
                    != Some(&IStablecoinFactory::StablecoinCreated::SIGNATURE_HASH)
        }));
    }

    let mut provider = super::DirectStorageProvider::new(&mut state, block_context);
    let storage = StorageHandle::new(&mut provider);
    let vote = Vote::new(storage.clone());
    let factory = StablecoinFactoryContract::new(storage.clone());
    assert_eq!(
        vote.proposals
            .get(U256::from(1u64))
            .unwrap()
            .unwrap()
            .proposal_status()
            .unwrap(),
        ProposalStatus::Error
    );
    assert_eq!(
        vote.proposal_bond(U256::from(1u64)).unwrap().settlement,
        BondSettlement::Refunded
    );
    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
    assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::ZERO);
    assert_eq!(factory.token_count().unwrap(), U256::ZERO);
    assert!(factory.reservations.exists(U256::from(1u64)).unwrap());
}
