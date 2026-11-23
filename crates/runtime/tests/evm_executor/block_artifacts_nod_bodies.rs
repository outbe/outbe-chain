//! Independent EVM states produce identical full-block results on both execution roles.

use super::*;
use outbe_system_common::time::{date_key_to_utc_timestamp, SECONDS_PER_DAY};

/// A non-midnight second on 2020-01-01. `first_full_day` can step that day.
fn issued_at_on_protocol_floor(second_of_day: u64) -> u64 {
    date_key_to_utc_timestamp(20_200_101).unwrap() + second_of_day
}

type NodBlockObservation = (
    B256,
    BundleState,
    Vec<Receipt>,
    U256,
    U256,
    u64,
    Vec<u64>,
    Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>>,
    u64,
);

fn nod_execution_context(
    proposer: Address,
    seed_hash: B256,
    parent_metadata: CertifiedParentAccountingMetadata,
    expected_system_txs: Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>>,
) -> OutbeBlockExecutionCtx<'static> {
    let mut execution = execution_ctx(Some(1), Bytes::new());
    execution.inner.parent_hash = seed_hash;
    execution.parent_consensus_metadata = Some(parent_metadata);
    execution.parent_artifact_hint = Some(AccountedParentArtifact {
        summary: ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        timestamp: 0,
        state_root: Some(B256::repeat_byte(0x91)),
    });
    execution.proposer_evm_address = Some(proposer);
    execution.expected_begin_system_txs = expected_system_txs;
    execution
}

fn assert_group_mutation_receipts(receipts: &[Receipt], expected_count: usize) {
    assert_eq!(receipts.len(), expected_count);
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.logs.iter().any(|log| {
                log.address == NOD_ADDRESS
                    && log.data.topics().first() == Some(&INod::NodSeriesClosed::SIGNATURE_HASH)
            })),
        "fixture must forfeit a Nod during the block"
    );
}

fn execute_nod_block(
    fixture: &NodBodyFixture,
    expected_validator_body: bool,
) -> Result<NodBlockObservation, BlockExecutionError> {
    outbe_chain_constants::initialize(None).unwrap();
    let proposer = fixture.proposer;

    let signer = test_evm_signer();
    let (mut state, seed_hash) = seed_nod_body_state(fixture).expect("seed Nod body fixture");
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let mut parent_metadata = metadata_with(vec![proposer], vec![1], Vec::new());
    parent_metadata.finalized_block_number = 1;
    parent_metadata.finalized_block_hash = seed_hash;
    let system_txs = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash: seed_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(parent_metadata.clone()),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    let mut visible_envelopes: Vec<u64> = system_txs.iter().map(|tx| tx.tx().gas_limit()).collect();
    let mut signed_body = system_txs.clone();
    let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
    let execution = nod_execution_context(
        proposer,
        seed_hash,
        parent_metadata,
        if expected_validator_body {
            system_txs.clone()
        } else {
            Vec::new()
        },
    );
    let mut executor = config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| {
        executor
            .apply_pre_execution_changes()
            .expect("state-backed pre-execution hook must succeed");
    });
    let mut cycle_internal_gas = None;
    for tx in system_txs {
        let is_cycle = matches!(
            SystemTx::decode(tx.tx().input().as_ref()).unwrap(),
            SystemTx::BeginBlock { step_index: 8 }
        );
        let internal_before = executor.system_tx_execution_gas;
        let result = executor.execute_transaction(tx);
        if result.is_err() {
            assert!(
                !executor.receipts().is_empty(),
                "the cancelled body read must follow earlier block transactions"
            );
        }
        result?;
        if is_cycle {
            let internal_gas = executor.system_tx_execution_gas - internal_before;
            let intrinsic = system_tx_intrinsic_gas(
                SystemTx::BeginBlock { step_index: 8 }
                    .encode()
                    .unwrap()
                    .as_ref(),
            )
            .unwrap();
            assert!(
                internal_gas > intrinsic,
                "CycleTick storage work must consume internal gas beyond intrinsic gas"
            );
            cycle_internal_gas = Some(internal_gas);
        }
        assert!(
            executor.system_tx_execution_gas
                <= outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT
        );
    }
    executor.apply_end_block_phase()?;
    let end_txs = executor.take_staged_end_block_txs();
    assert_eq!(end_txs.len(), 5);
    visible_envelopes.extend(end_txs.iter().map(|tx| tx.tx().gas_limit()));
    signed_body.extend(end_txs);
    let receipts = executor.receipts().to_vec();
    assert_group_mutation_receipts(&receipts, signed_body.len());
    fixtures_execution::finalize_executor_header_artifacts(&mut executor, 0)
        .expect("final extra_data should encode");
    let output = executor.finish().expect("block finish must succeed");
    let block_result = output.result;
    let bundle = output.state;
    state.commit_source(&BundleSource(&bundle));
    let root = post_state_root(&bundle);
    let proposer_balance = signer_balance(&mut state, proposer);
    let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);

    Ok::<_, BlockExecutionError>((
        root,
        bundle,
        receipts,
        proposer_balance,
        rewards_balance,
        block_result.gas_used,
        visible_envelopes,
        signed_body,
        cycle_internal_gas.expect("CycleTick internal gas must be captured"),
    ))
}

#[test]
fn independent_evm_states_produce_identical_full_block_state_receipts_and_balances() {
    let proposer = test_evm_signer().address();
    let worldwide_day = WorldwideDay::new(20_241_220);
    let entry_price_minor = U256::from(450_000_000u64);
    let issued_at = issued_at_on_protocol_floor(1);
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::TESTNET,
        issued_at,
        entry_price_minor,
        840,
    );
    let nod_item = || {
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id: outbe_nod::test_support::nod_id_of(
                    proposer,
                    worldwide_day.value(),
                    840,
                    840,
                ),
                owner: proposer,
                gratis_load_minor: U256::from(1_000_000u64),
                emission_day: worldwide_day.value(),
                league_id: 1,
                group_key,
                issuance_currency: 840,
                reference_currency: 840,
                issued_at,
            },
            entry_price_minor,
        )
    };
    let fixture = NodBodyFixture {
        proposer,
        group_key,
        item: nod_item(),
    };

    let proposer_result =
        execute_nod_block(&fixture, false).expect("proposer block execution succeeds");
    let validator_result = execute_nod_block(&fixture, true).expect("canonical replay succeeds");
    assert_eq!(proposer_result, validator_result);
    assert_call_event_receipt_gas(&proposer_result.2, proposer_result.5, &proposer_result.6);
}

fn assert_call_event_receipt_gas(receipts: &[Receipt], gas_used: u64, envelopes: &[u64]) {
    let body_receipt_index = receipts
        .iter()
        .position(|receipt| {
            receipt.logs.iter().any(|log| {
                log.address == NOD_ADDRESS
                    && log.data.topics().first() == Some(&INod::NodSeriesClosed::SIGNATURE_HASH)
            })
        })
        .expect("NodFactory body mutation receipt");
    assert_eq!(body_receipt_index, receipts.len() - 5);
    assert!(receipts[body_receipt_index].success);
    let previous_cumulative = body_receipt_index
        .checked_sub(1)
        .map_or(0, |index| receipts[index].cumulative_gas_used);
    let body_receipt_gas = receipts[body_receipt_index]
        .cumulative_gas_used
        .saturating_sub(previous_cumulative);
    assert_eq!(body_receipt_gas, 0);
    assert_eq!(envelopes[body_receipt_index], 0);
    assert_eq!(
        gas_used,
        receipts.last().unwrap().cumulative_gas_used,
        "header gas_used must equal the final receipt cumulative gas"
    );
}

#[test]
fn proposer_validator_state_mints_and_rollbacks_match() {
    let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let proposer = test_evm_signer().address();
    let day = WorldwideDay::new(20_260_716);
    let tribute_owner = Address::repeat_byte(0x31);
    let tribute_id =
        outbe_core_common::wwd_entity_id::derive_poseidon_entity_id(tribute_owner, day);
    let nod_owner = Address::repeat_byte(0x32);
    let nod_id = outbe_nod::test_support::nod_id_of(nod_owner, day.value(), 840, 978);
    let issued_at = issued_at_on_protocol_floor(15);
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::TESTNET,
        issued_at,
        U256::from(16),
        978,
    );
    let ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer,
        validators: vec![proposer],
    };

    let tribute_fixture = || TributeData {
        tribute_id,
        owner: tribute_owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
    };
    let nod_fixture = || {
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id,
                owner: nod_owner,
                gratis_load_minor: U256::from(1),
                emission_day: day.value(),
                league_id: 2,
                group_key,
                issuance_currency: 840,
                reference_currency: 978,
                issued_at,
            },
            U256::from(16),
        )
    };

    let run = || {
        arm_direct_process_ready_budget();
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
        let (changes, events) =
            super::run_atomic_storage_hooks(&mut state, ctx.clone(), |hook_ctx| {
                let tribute = tribute_fixture();
                let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
                with_process_ready_frame(&hook_ctx.storage, || {
                    tribute_contract.issue(&tribute)?;
                    outbe_nod::test_support::add(&hook_ctx.storage, &nod_fixture())?;
                    Ok(())
                })
            })
            .expect("body mint execution must succeed");
        let proposer_balance = signer_balance(&mut state, proposer);
        let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);
        (changes, events, proposer_balance, rewards_balance)
    };

    let proposer_result = run();
    let validator_result = run();
    assert_eq!(proposer_result, validator_result);
    assert!(proposer_result.1.iter().any(|event| {
        event.address == outbe_system_common::addresses::TRIBUTE_ADDRESS
            && event.data.topics()[0]
                == outbe_tribute::precompile::ITribute::TributeOffered::SIGNATURE_HASH
    }));
    assert!(proposer_result.1.iter().any(|event| {
        event.address == NOD_ADDRESS && event.data.topics()[0] == INod::Transfer::SIGNATURE_HASH
    }));

    arm_direct_process_ready_budget();
    let mut failed_state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
    let error = super::run_atomic_storage_hooks(&mut failed_state, ctx.clone(), |hook_ctx| {
        let tribute = tribute_fixture();
        let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
        with_process_ready_frame(&hook_ctx.storage, || {
            tribute_contract.issue(&tribute)?;
            outbe_nod::test_support::add(&hook_ctx.storage, &nod_fixture())?;
            Err(outbe_primitives::error::PrecompileError::Fatal(
                "later transaction stage failed".into(),
            ))
        })
    })
    .expect_err("failed transaction must roll back every body namespace");
    assert!(error.to_string().contains("later transaction stage failed"));
    let mut read_provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut failed_state, ctx);
    StorageHandle::enter(&mut read_provider, |storage| {
        let tribute = tribute_fixture();
        assert!(TributeContract::new(storage.clone())
            .get_record(tribute.tribute_id)?
            .is_none());
        assert!(outbe_nod::api::get_item(&storage, nod_fixture().nod_id)?.is_none());
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .unwrap();
}

struct NodBodyFixture {
    proposer: Address,
    group_key: B256,
    item: NodItemState,
}
type SeededNodParent = (CacheDB<EmptyDB>, B256);
fn arm_direct_process_ready_budget() {
    outbe_primitives::storage::direct::arm_next_direct_scoped_frame_budget(
        outbe_primitives::storage::MutationPurposeTag::CycleLifecycle,
        1,
    );
}

fn with_process_ready_frame(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    body: impl FnOnce() -> outbe_primitives::error::Result<()>,
) -> outbe_primitives::error::Result<()> {
    let binding = outbe_system_common::metadosis_bindings::metadosis_process_ready_binding(
        storage.chain_id()?,
        storage.block_number()?,
        storage.now_secs()?,
    );
    storage
        .with_scoped_mutation::<outbe_primitives::storage::CycleLifecyclePurpose, _>(binding, body)
}

fn seed_nod_body_state(fixture: &NodBodyFixture) -> eyre::Result<SeededNodParent> {
    let mut seeded = Ok(());
    let state = state_with_active_validators_seeded_at_block(
        &[(fixture.proposer, dummy_pubkey(0xA2))],
        1,
        |storage| {
            seeded = seed_called_nod(storage, fixture);
        },
    );
    seeded?;
    Ok((state, B256::repeat_byte(0x41)))
}

fn seed_called_nod(storage: StorageHandle<'_>, fixture: &NodBodyFixture) -> eyre::Result<()> {
    let group_key = fixture.group_key;
    outbe_nod::test_support::add(&storage, &fixture.item)?;
    // NodFactory forfeits the lapsed called group and deletes both bodies.
    let nod = NodContract::new(storage.clone());
    nod.group_called_at.write(&group_key, 1)?;
    let notice = nod.group_call_notice_period_seconds.read(&group_key)?;
    outbe_core_common::deadline_queue::push(
        &outbe_nod::state::ForfeitQueue(&nod),
        group_key,
        outbe_nod::api::settlement_deadline_of(1, notice),
    )?;
    seed_nod_daily_task(storage, fixture)?;

    Ok(())
}

fn seed_nod_daily_task(storage: StorageHandle<'_>, _fixture: &NodBodyFixture) -> eyre::Result<()> {
    let (.., pair_index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840)?;
    let previous_day = outbe_system_common::time::previous_date_key(
        outbe_system_common::time::timestamp_to_date_key(TEST_BLOCK_TIMESTAMP_BASE + 2),
    )
    .expect("date step");
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    oracle.record_utc_day_vwap(previous_day, pair_index, U256::from(1_000_000u64))?;
    oracle.utc_day_vwap_last_finalized.write(previous_day)?;
    outbe_cycle::schema::Cycle::new(storage.clone())
        .last_executed_at
        .write(
            &outbe_cycle::tasks::TaskId::ScheduleNodCallPass.as_u32(),
            TEST_BLOCK_TIMESTAMP_BASE - SECONDS_PER_DAY,
        )?;

    Ok(())
}

struct WitnessedForfeitBlock {
    extra_data: Bytes,
    end_txs: Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>>,
    receipts: Vec<Receipt>,
    root: B256,
}

struct ForfeitBlockSetup {
    state: CacheDB<EmptyDB>,
    config: OutbeEvmConfig,
    begin: Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>>,
    parent_metadata: CertifiedParentAccountingMetadata,
}

const FORFEIT_SEED_HASH: B256 = B256::repeat_byte(0x41);

fn forfeit_block_setup(
    fixture: &NodBodyFixture,
    leader: &std::sync::Arc<outbe_tee::in_process::InProcessTeeContext>,
) -> ForfeitBlockSetup {
    outbe_chain_constants::initialize(None).unwrap();
    let proposer = fixture.proposer;
    let leader_public = leader.attestation_public();
    let state = {
        // Seeding seals the group load under the resident state key, outside any block policy.
        let policy = outbe_tee::witness::witness_policy();
        outbe_tee::witness::set_witness_policy_for_test(None);
        outbe_nod::test_support::uninstall();
        let _seed_runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(
            Some(std::sync::Arc::clone(leader)),
        );
        let mut seeded = Ok(());
        let state = state_with_active_validators_seeded_at_block(
            &[(proposer, dummy_pubkey(0xA2))],
            1,
            |storage| {
                super::seed_valid_tee_nodes(storage.clone(), &[proposer]);
                let registry = outbe_teeregistry::TeeRegistry::new(storage.clone());
                let node_hash = registry.validator_v1_node_hash.read(&proposer).unwrap();
                registry
                    .v1_node_attestation_ed25519
                    .write(&node_hash, B256::from(leader_public))
                    .unwrap();
                seeded = seed_called_nod(storage, fixture);
            },
        );
        seeded.expect("seed the lapsed called group");
        drop(_seed_runtime);
        outbe_tee::witness::set_witness_policy_for_test(policy);
        state
    };
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(test_evm_signer());
    let mut parent_metadata = metadata_with(vec![proposer], vec![1], Vec::new());
    parent_metadata.finalized_block_number = 1;
    parent_metadata.finalized_block_hash = FORFEIT_SEED_HASH;
    let begin = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash: FORFEIT_SEED_HASH,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(parent_metadata.clone()),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );
    ForfeitBlockSetup {
        state,
        config,
        begin,
        parent_metadata,
    }
}

fn forfeit_execution_context(
    setup: &ForfeitBlockSetup,
    proposer: Address,
    proposed: Option<&WitnessedForfeitBlock>,
) -> OutbeBlockExecutionCtx<'static> {
    let mut execution = nod_execution_context(
        proposer,
        FORFEIT_SEED_HASH,
        setup.parent_metadata.clone(),
        match proposed {
            Some(_) => setup.begin.clone(),
            None => Vec::new(),
        },
    );
    if let Some(proposed) = proposed {
        execution.inner.extra_data = proposed.extra_data.clone();
        execution.inner.tx_count_hint = Some(setup.begin.len() + proposed.end_txs.len());
        execution.block_hash = Some(B256::repeat_byte(0x9A));
    }
    execution
}

/// Executes the forfeit block as its proposer, or as a verifier of `proposed` from its body.
fn execute_witnessed_forfeit_block(
    fixture: &NodBodyFixture,
    leader: &std::sync::Arc<outbe_tee::in_process::InProcessTeeContext>,
    proposed: Option<&WitnessedForfeitBlock>,
) -> WitnessedForfeitBlock {
    let mut setup = forfeit_block_setup(fixture, leader);
    let execution = forfeit_execution_context(&setup, fixture.proposer, proposed);
    let evm = setup
        .config
        .evm_with_env(&mut setup.state, test_evm_env(2, REWARDS_ADDRESS));
    let mut executor = setup.config.create_executor(evm, execution);
    super::with_phase1_verify_disabled(|| executor.apply_pre_execution_changes().unwrap());
    for tx in setup.begin.clone() {
        executor.execute_transaction(tx).unwrap();
    }
    let end_txs = match proposed {
        None => {
            executor.apply_end_block_phase().unwrap();
            let end_txs = executor.take_staged_end_block_txs();
            fixtures_execution::finalize_executor_header_artifacts(&mut executor, 0).unwrap();
            end_txs
        }
        Some(proposed) => {
            for tx in proposed.end_txs.clone() {
                executor.execute_transaction(tx).unwrap();
            }
            proposed.end_txs.clone()
        }
    };
    let extra_data = executor.final_extra_data().clone();
    let output = executor.finish().unwrap();
    let result = output.result;
    WitnessedForfeitBlock {
        extra_data,
        end_txs,
        receipts: result.receipts,
        root: post_state_root(&output.state),
    }
}

/// Traces every body transaction of `proposed` the way a node answers `debug_traceBlock`.
fn trace_witnessed_forfeit_block(
    fixture: &NodBodyFixture,
    leader: &std::sync::Arc<outbe_tee::in_process::InProcessTeeContext>,
    proposed: &WitnessedForfeitBlock,
) {
    let mut setup = forfeit_block_setup(fixture, leader);
    {
        let execution = forfeit_execution_context(&setup, fixture.proposer, Some(proposed));
        let evm = setup
            .config
            .evm_with_env(&mut setup.state, test_evm_env(2, REWARDS_ADDRESS));
        let mut executor = setup.config.create_executor(evm, execution);
        super::with_phase1_verify_disabled(|| executor.apply_pre_execution_changes().unwrap());
    }
    let mut evm = setup
        .config
        .evm_with_env(&mut setup.state, test_evm_env(2, REWARDS_ADDRESS));
    for tx in setup.begin.iter().chain(&proposed.end_txs) {
        let traced = evm
            .transact_with_inspector(
                &tx_env(&setup.config, tx),
                &mut evm2::inspector::NoopInspector::default(),
            )
            .expect("the traced transaction replays its witness")
            .commit();
        assert!(traced.is_success());
    }
    outbe_evm::executor::witness::clear_detached_trace_witness();
}

/// The forfeit's private read rides the end-block witness: the proposer records it at the
/// NodFactory step, an enclave validator checks it, and a keyless follower replays and traces it.
#[test]
fn the_forfeit_read_rides_the_end_block_witness_in_every_mode() {
    let leader = super::witness_parity::resident_context(1);
    let verifier = super::witness_parity::resident_context(2);
    let nod_state_key = outbe_core_common::confidential_tee::derive_resident_state_key(
        &leader,
        &outbe_core_common::confidential_tee::NOD,
        0,
    )
    .expect("nod state key");
    let proposer = test_evm_signer().address();
    let worldwide_day = WorldwideDay::new(20_241_220);
    let entry_price_minor = U256::from(450_000_000u64);
    let issued_at = issued_at_on_protocol_floor(1);
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::TESTNET,
        issued_at,
        entry_price_minor,
        840,
    );
    let nod_id = outbe_nod::test_support::nod_id_of(proposer, worldwide_day.value(), 840, 840);
    let mut item = {
        let _enclave = outbe_nod::test_support::scope();
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id,
                owner: proposer,
                gratis_load_minor: U256::from(1_000_000u64),
                emission_day: worldwide_day.value(),
                league_id: 1,
                group_key,
                issuance_currency: 840,
                reference_currency: 840,
                issued_at,
            },
            entry_price_minor,
        )
    };
    item.encrypted = outbe_nod::nod_encryption::encrypt_nod(
        &nod_state_key,
        outbe_core_common::nod_encryption::NodTermsV1 {
            chain_id: CHAIN_ID,
            nod_id,
            owner: proposer,
            emission_day: worldwide_day.value(),
            league_id: 1,
            entry_price_minor,
            issuance_currency: 840,
            reference_currency: 840,
        },
        U256::from(1_000_000u64),
    )
    .expect("seal the Nod under the resident state key");
    outbe_nod::test_support::uninstall();
    let fixture = NodBodyFixture {
        proposer,
        group_key,
        item,
    };

    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::EnclaveBacked,
    ));
    let leader_runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
        std::sync::Arc::clone(&leader),
    ));
    let proposed = execute_witnessed_forfeit_block(&fixture, &leader, None);
    assert_group_mutation_receipts(&proposed.receipts, proposed.receipts.len());
    let witness = decode_outbe_block_artifacts(proposed.extra_data.as_ref())
        .unwrap()
        .enclave_witness
        .expect("the forfeit read is on the tape");
    let tape = decode_witness_payload_v1(&witness).unwrap();
    let nod_factory_index = (proposed.receipts.len() - proposed.end_txs.len()) as u32;
    assert!(tape
        .txs
        .iter()
        .any(|tx| tx.tx_index == nod_factory_index && !tx.entries.is_empty()));

    drop(leader_runtime);
    let verifier_runtime =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(verifier));
    let checked = execute_witnessed_forfeit_block(&fixture, &leader, Some(&proposed));
    assert_eq!(
        (&checked.receipts, checked.root),
        (&proposed.receipts, proposed.root)
    );

    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::ReplayOnly,
    ));
    drop(verifier_runtime);
    let _absent = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(None);
    let replayed = execute_witnessed_forfeit_block(&fixture, &leader, Some(&proposed));
    assert_eq!(
        (&replayed.receipts, replayed.root),
        (&proposed.receipts, proposed.root)
    );
    trace_witnessed_forfeit_block(&fixture, &leader, &proposed);

    outbe_tee::witness::set_witness_policy_for_test(None);
}
