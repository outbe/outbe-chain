//! System receipts that publish module events directly: slashing, update activation and factory proposals.

use super::*;
use outbe_primitives::block::ProtocolModule;

#[test]
fn l2_registration_conflict_is_atomic_with_the_outer_hook_batch() {
    use outbe_l2registry::L2RegistryContract;
    use outbe_primitives::{addresses::L2_REGISTRY_ADDRESS, error::PrecompileError};
    use outbe_vote::schema::{BondState, ProposalStatus};

    const CREATED: u64 = 7;
    const NETWORK_CHAIN_ID: u64 = 4242;
    const PAYLOAD: &str = r#"{"operation":"register","chainId":4242,"l1Address":"0x1111111111111111111111111111111111111111","publicKey":"0x"}"#;
    let validators = [
        (Address::repeat_byte(0xb1), dummy_pubkey(0xb1)),
        (Address::repeat_byte(0xb2), dummy_pubkey(0xb2)),
        (Address::repeat_byte(0xb3), dummy_pubkey(0xb3)),
    ];
    let proposer = validators[0].0;
    let existing_owner = Address::repeat_byte(0xc1);
    let id = U256::ONE;
    let mut state = state_with_active_validators_seeded_at_block(&validators, CREATED, |storage| {
        let mut vote = Vote::new(storage.clone());
        assert_eq!(
            vote.create_proposal(
                proposer,
                L2_REGISTRY_ADDRESS,
                PAYLOAD,
                CREATED,
                outbe_vote::handlers::registry(),
            )
            .unwrap(),
            id
        );
        for (voter, _) in &validators[..2] {
            vote.cast_vote_approve(id, *voter, true, CREATED + 1)
                .unwrap();
        }
        // The proposal is valid, but its registration conflicts by execution time.
        L2RegistryContract::new(storage)
            .register_network(NETWORK_CHAIN_ID, existing_owner, &[])
            .unwrap();
    });
    let number = CREATED + VOTING_WINDOW_BLOCKS + 1;
    let context = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: number,
        timestamp: 1_700_000_000,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1_700_000_000,
        proposer,
        validators: validators.iter().map(|(address, _)| *address).collect(),
    };

    super::run_atomic_storage_hooks(&mut state, context.clone(), |ctx| {
        outbe_vote::VoteModule::begin_block(ctx)?;
        let vote = Vote::new(ctx.storage.clone());
        assert_eq!(
            vote.proposals.get(id)?.unwrap().proposal_status()?,
            ProposalStatus::Error
        );
        assert!(vote.list_pending_proposal_ids()?.is_empty());
        Err(PrecompileError::Fatal("failure after Vote tally".into()))
    })
    .expect_err("a later hook failure must roll back the terminal target refusal");

    let (_, events) = super::run_atomic_storage_hooks(&mut state, context, |ctx| {
        let vote = Vote::new(ctx.storage.clone());
        assert_eq!(
            vote.proposals.get(id)?.unwrap().proposal_status()?,
            ProposalStatus::Pending
        );
        assert_eq!(vote.list_pending_proposal_ids()?, vec![id]);
        outbe_vote::VoteModule::begin_block(ctx)?;
        assert_eq!(
            vote.proposals.get(id)?.unwrap().proposal_status()?,
            ProposalStatus::Error
        );
        assert_eq!(vote.proposal_bond(id)?.state, BondState::NoBond);
        assert!(vote.list_pending_proposal_ids()?.is_empty());
        assert_eq!(
            L2RegistryContract::new(ctx.storage.clone())
                .load_network(NETWORK_CHAIN_ID)?
                .l1_address,
            existing_owner
        );
        // Re-running the Vote lifecycle must not emit a second ProposalErrored receipt.
        outbe_vote::lifecycle::VoteLifecycle::begin_block_with_handlers(
            ctx,
            outbe_vote::handlers::registry(),
        )?;
        Ok(())
    })
    .expect("target refusal must allow the hook batch to commit on retry");
    assert_eq!(
        events
            .iter()
            .filter(|log| log.address == VOTE_ADDRESS
                && log.data.topics().first() == Some(&IVote::ProposalErrored::SIGNATURE_HASH))
            .count(),
        1
    );
    assert!(events.iter().all(|log| log.address != L2_REGISTRY_ADDRESS));
}

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

    assert_eq!(executor.receipts().len(), 12);
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

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 2,
        timestamp: 2,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 2,
        proposer,
        validators: vec![proposer, absent],
    };
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
fn update_system_receipt_carries_update_activation_log() {
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

    let receipt = execute_begin_block_system_tx(
        &mut state,
        ACTIVATION_BLOCK,
        ACTIVATION_BLOCK,
        proposer,
        SystemTx::Update,
    );
    let upgrade_activated = IUpdate::UpgradeActivated::SIGNATURE_HASH;
    assert!(
        receipt.logs.iter().any(|log| {
            log.address == UPDATE_ADDRESS && log.data.topics().first() == Some(&upgrade_activated)
        }),
        "Update system transaction must emit UpgradeActivated directly"
    );
    assert_eq!(receipt.cumulative_gas_used, 0);
}

#[test]
fn real_factory_approval_is_published_in_vote_system_receipt() {
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
                    outbe_vote::handlers::registry(),
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
    let block_context = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: finalization_block,
        timestamp: 1_700_000_000,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1_700_000_000,
        proposer: issuer,
        validators: validators.iter().map(|(address, _)| *address).collect(),
    };
    let (_, receipt_logs) =
        super::run_atomic_storage_hooks(&mut state, block_context.clone(), |hook_ctx| {
            outbe_vote::VoteModule::begin_block(hook_ctx)
        })
        .expect("typed target Error must not fail the outer hook batch");
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
        vote.proposal_bond(U256::from(1u64)).unwrap().state,
        BondState::Refunded
    );
    assert_eq!(vote.bond_liabilities().unwrap(), U256::ZERO);
    assert_eq!(storage.balance(VOTE_ADDRESS).unwrap(), U256::ZERO);
    assert_eq!(factory.token_count().unwrap(), U256::ZERO);
    assert!(factory.reservations.exists(U256::from(1u64)).unwrap());
}

/// Automatic retirement emits `UnbondingStarted` at the staking address
/// directly in the StakingModule begin-block pass.
#[test]
fn automatic_retirement_unbonding_started_is_published_in_staking_system_receipt() {
    use alloy_primitives::BloomInput;
    use alloy_sol_types::SolEvent;
    use outbe_staking::contract::Staking;
    use outbe_staking::precompile::IStaking;
    use outbe_validatorset::contract::ValidatorSet;
    use outbe_validatorset::ValidatorLifecycle;

    const SEED_BLOCK: u64 = 20;
    const HOOK_BLOCK: u64 = 21;
    const TIMESTAMP: u64 = 1_000;
    const PERIOD: u64 = 3_600;
    const STAKE: u64 = 1_000;
    // A stored slashed-withdrawal delay of 0 uses twice the unbonding period.
    const COMPLETE_TIME: u64 = TIMESTAMP + 2 * PERIOD;

    let proposer = Address::repeat_byte(0x31);
    let subject = Address::repeat_byte(0x32);
    let mut state = state_with_active_validators_seeded_at_block(
        &[
            (proposer, dummy_pubkey(0x31)),
            (subject, dummy_pubkey(0x32)),
        ],
        SEED_BLOCK,
        |storage| {
            let mut staking = Staking::new(storage.clone());
            staking
                .config_min_stake
                .write(U256::from(STAKE))
                .expect("setup: min stake");
            staking
                .config_unbonding_period
                .write(PERIOD)
                .expect("setup: unbonding period");
            staking
                .stake_amount
                .write(&proposer, U256::ZERO)
                .expect("setup: reset proposer stake");
            staking
                .stake_amount
                .write(&subject, U256::ZERO)
                .expect("setup: reset subject stake");
            staking
                .total_staked
                .write(U256::ZERO)
                .expect("setup: reset total staked");
            staking
                .topup(proposer, proposer, U256::from(STAKE))
                .expect("setup: proposer stake");
            staking
                .topup(subject, subject, U256::from(STAKE))
                .expect("setup: subject stake");
            storage
                .set_balance(STAKING_ADDRESS, U256::from(STAKE) * U256::from(2u64))
                .expect("setup: staking escrow");
            let mut validators = ValidatorSet::new(storage);
            validators
                .force_exit_validator(subject)
                .expect("setup: force exit");
            validators
                .test_activate_validated_boundary_set(
                    &[proposer],
                    B256::with_last_byte(0x11),
                    SEED_BLOCK,
                )
                .expect("setup: boundary");
            assert!(
                matches!(
                    validators
                        .validator_lifecycle(subject)
                        .expect("setup: subject lifecycle"),
                    ValidatorLifecycle::Unbonding(_)
                ),
                "setup: subject must be Unbonding before the hook"
            );
            assert!(
                matches!(
                    validators
                        .validator_lifecycle(proposer)
                        .expect("setup: proposer lifecycle"),
                    ValidatorLifecycle::Active(_)
                ),
                "setup: proposer stays Active"
            );
        },
    );

    let ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: HOOK_BLOCK,
        timestamp: TIMESTAMP,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: TIMESTAMP.max(1),
        proposer,
        validators: vec![proposer, subject],
    };
    let receipt = execute_begin_block_system_tx(
        &mut state,
        HOOK_BLOCK,
        TIMESTAMP,
        proposer,
        SystemTx::Staking,
    );

    let started: Vec<_> = receipt
        .logs
        .iter()
        .filter(|log| {
            log.address == STAKING_ADDRESS
                && log.data.topics().first() == Some(&IStaking::UnbondingStarted::SIGNATURE_HASH)
        })
        .collect();
    assert_eq!(
        started.len(),
        1,
        "setup: the core hook must emit one UnbondingStarted before the receipt check"
    );
    let decoded = IStaking::UnbondingStarted::decode_log_data(started[0])
        .expect("setup: UnbondingStarted must decode");
    assert_eq!(
        decoded,
        IStaking::UnbondingStarted {
            validator: subject,
            index: 0,
            amount: U256::from(STAKE),
            completeTime: COMPLETE_TIME,
            automatic: true,
        }
    );
    assert!(receipt.logs.iter().all(|log| {
        log.address != STAKING_ADDRESS
            || log.data.topics().first() != Some(&IStaking::Slashed::SIGNATURE_HASH)
    }));

    let mut provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let staking = Staking::new(storage.clone());
        assert_eq!(staking.stake_amount.read(&subject)?, U256::ZERO);
        assert_eq!(
            staking.unbonding_amount.read(&decoded.index)?,
            decoded.amount
        );
        assert_eq!(
            staking.unbonding_complete_time.read(&decoded.index)?,
            decoded.completeTime
        );
        assert_eq!(staking.total_staked.read()?, U256::from(STAKE));
        assert_eq!(
            storage.balance(STAKING_ADDRESS)?,
            U256::from(STAKE) * U256::from(2u64)
        );
        let validators = ValidatorSet::new(storage);
        assert!(matches!(
            validators.validator_lifecycle(proposer)?,
            ValidatorLifecycle::Active(_)
        ));
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("queue state should match the retirement event");

    let is_started = |log: &Log| {
        log.address == STAKING_ADDRESS
            && log.data.topics().first() == Some(&IStaking::UnbondingStarted::SIGNATURE_HASH)
    };
    assert_eq!(receipt.cumulative_gas_used, 0);
    let published = receipt
        .logs
        .iter()
        .find(|log| is_started(log))
        .expect("receipt must contain UnbondingStarted");
    assert_eq!(
        IStaking::UnbondingStarted::decode_log_data(published).expect("receipt log must decode"),
        decoded
    );
    let bloom = logs_bloom(receipt.logs.iter());
    assert!(bloom.contains_input(BloomInput::Raw(published.address.as_slice())));
    for topic in published.data.topics() {
        assert!(bloom.contains_input(BloomInput::Raw(topic.as_slice())));
    }
}

fn execute_begin_block_system_tx(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    block_number: u64,
    timestamp: u64,
    proposer: Address,
    input: SystemTx,
) -> Receipt {
    use alloy_evm::Evm as _;
    use revm::DatabaseCommit;

    let mut evm_env = test_evm_env(block_number, REWARDS_ADDRESS);
    evm_env.block_env.timestamp = U256::from(timestamp);
    let config = OutbeEvmConfig::new(test_chain_spec());
    let result = {
        let mut evm = config.evm_with_env(&mut *state, evm_env);
        crate::sys_tx_precompile::with_preloaded_system_tx_context(
            crate::sys_tx_precompile::PreloadedSystemTxContext {
                proposer,
                finalized_summary: None,
                allow_boundary_proposer: false,
                canonical_vrf_proof_hash: B256::ZERO,
                verified_late_credit_signers: None,
                gas_limit_override: None,
                remaining_system_gross_gas:
                    outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT,
                block_gas_limit: 0,
                user_gas_used: 0,
            },
            || {
                evm.transact_system_call(
                    outbe_primitives::addresses::SYSTEM_ADDRESS,
                    OUTBE_SYSTEM_TX_ADDRESS,
                    input.encode().expect("begin-block system tx encodes"),
                )
            },
        )
        .expect("begin-block system call")
    };
    assert!(
        result.result.is_success(),
        "begin-block system call must succeed"
    );
    let logs = result.result.into_logs();
    state.commit(result.state);
    Receipt {
        tx_type: alloy_consensus::TxType::Legacy,
        success: true,
        cumulative_gas_used: 0,
        logs,
    }
}
