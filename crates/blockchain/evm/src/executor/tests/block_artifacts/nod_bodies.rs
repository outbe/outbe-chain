//! Independent EVM states produce identical full-block results on both execution roles.

use super::*;
use outbe_primitives::time::SECONDS_PER_DAY;

type NodBlockObservation = (
    B256,
    revm::database::BundleState,
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
                    && log.data.topics().first() == Some(&INod::NodGroupForfeited::SIGNATURE_HASH)
            })),
        "fixture must forfeit a Nod during the block"
    );
}

fn execute_nod_block(
    fixture: &NodBodyFixture,
    expected_validator_body: bool,
) -> Result<NodBlockObservation, alloy_evm::block::BlockExecutionError> {
    outbe_chain_constants::initialize(None).unwrap();
    let proposer = fixture.proposer;

    let signer = test_evm_signer();
    let (mut state, seed_hash) = seed_nod_body_state(fixture).expect("seed Nod body fixture");
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer);
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
    let visible_envelopes: Vec<u64> = system_txs.iter().map(|tx| tx.tx().gas_limit()).collect();
    let signed_body = system_txs.clone();
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
            SystemTxInputV2::decode(tx.tx().input().as_ref()).unwrap(),
            SystemTxInputV2::CycleTick
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
            let intrinsic =
                system_tx_intrinsic_gas(SystemTxInputV2::CycleTick.encode().unwrap().as_ref())
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
    let receipts = executor.receipts().to_vec();
    assert_group_mutation_receipts(&receipts, signed_body.len());
    executor
        .prepare_final_header_artifacts(0)
        .expect("final extra_data should encode");
    let (evm, block_result) = executor.finish().expect("block finish must succeed");
    drop(evm);
    let bundle = state.bundle_state.clone();
    let root = post_state_root(&bundle);
    let proposer_balance = signer_balance(&mut state, proposer);
    let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);

    Ok::<_, alloy_evm::block::BlockExecutionError>((
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
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::DEV,
        1,
        entry_price_minor,
        840,
    );
    let nod_item = || {
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id: outbe_nod::identity::generate_nod_id(proposer, worldwide_day).unwrap(),
                owner: proposer,
                gratis_load_minor: U256::from(1_000_000u64),
                worldwide_day,
                league_id: 1,
                group_key,
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: 1,
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
    assert_cycle_tick_receipt_gas(&proposer_result.2, proposer_result.5, &proposer_result.6);
}

fn assert_cycle_tick_receipt_gas(receipts: &[Receipt], gas_used: u64, envelopes: &[u64]) {
    let body_receipt_index = receipts
        .iter()
        .position(|receipt| {
            receipt.logs.iter().any(|log| {
                log.address == NOD_ADDRESS
                    && log.data.topics().first() == Some(&INod::NodGroupForfeited::SIGNATURE_HASH)
            })
        })
        .expect("CycleTick body mutation receipt");
    let previous_cumulative = body_receipt_index
        .checked_sub(1)
        .map_or(0, |index| receipts[index].cumulative_gas_used);
    let body_receipt_gas = receipts[body_receipt_index]
        .cumulative_gas_used
        .saturating_sub(previous_cumulative);
    let cycle_intrinsic_gas =
        system_tx_intrinsic_gas(SystemTxInputV2::CycleTick.encode().unwrap().as_ref()).unwrap();
    assert_eq!(
        body_receipt_gas, cycle_intrinsic_gas,
        "native storage work uses the internal budget"
    );
    assert!(
        body_receipt_gas <= envelopes[body_receipt_index],
        "receipt-visible CycleTick gas must not exceed its signed gas limit"
    );
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
        outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(tribute_owner, day).unwrap();
    let nod_owner = Address::repeat_byte(0x32);
    let nod_id =
        outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(nod_owner, day).unwrap();
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::DEV,
        15,
        U256::from(16),
        978,
    );
    let ctx = BlockContext::new(1, 1, CHAIN_ID, proposer, vec![proposer]);

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
                worldwide_day: day,
                league_id: 2,
                group_key,
                issuance_currency: 840,
                reference_currency: 978,
                issued_at: 15,
            },
            U256::from(16),
        )
    };

    let run = || {
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
        let (changes, events) =
            super::run_atomic_storage_hooks(&mut state, ctx.clone(), |hook_ctx| {
                let tribute = tribute_fixture();
                let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
                tribute_contract.unseal_day(day)?;
                tribute_contract.issue(&tribute)?;
                outbe_nod::test_support::add(&hook_ctx.storage, &nod_fixture())?;
                Ok(())
            })
            .expect("body mint execution must succeed");
        let root = post_state_root(&state.bundle_state);
        let proposer_balance = signer_balance(&mut state, proposer);
        let rewards_balance = signer_balance(&mut state, REWARDS_ADDRESS);
        (
            changes,
            events,
            root,
            state.bundle_state,
            proposer_balance,
            rewards_balance,
        )
    };

    let proposer_result = run();
    let validator_result = run();
    assert_eq!(proposer_result, validator_result);
    assert!(proposer_result.1.iter().any(|event| {
        event.address == outbe_primitives::addresses::TRIBUTE_ADDRESS
            && event.data.topics()[0]
                == outbe_tribute::precompile::ITribute::TributeIssued::SIGNATURE_HASH
    }));
    assert!(proposer_result.1.iter().any(|event| {
        event.address == NOD_ADDRESS && event.data.topics()[0] == INod::Transfer::SIGNATURE_HASH
    }));

    let mut failed_state =
        state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |_| {});
    let error = super::run_atomic_storage_hooks(&mut failed_state, ctx.clone(), |hook_ctx| {
        let tribute = tribute_fixture();
        let mut tribute_contract = TributeContract::new(hook_ctx.storage.clone());
        tribute_contract.unseal_day(day)?;
        tribute_contract.issue(&tribute)?;
        outbe_nod::test_support::add(&hook_ctx.storage, &nod_fixture())?;
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "later transaction stage failed".into(),
        ))
    })
    .expect_err("failed transaction must roll back every body namespace");
    assert!(error.to_string().contains("later transaction stage failed"));
    let mut read_provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut failed_state, ctx);
    StorageHandle::enter(&mut read_provider, |storage| {
        assert_eq!(TributeContract::new(storage.clone()).total_supply()?, 0);
        assert_eq!(NodContract::new(storage).total_supply()?, 0);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .unwrap();
}

struct NodBodyFixture {
    proposer: Address,
    group_key: B256,
    item: NodItemState,
}
type SeededNodParent = (State<CacheDB<EmptyDBTyped<ProviderError>>>, B256);
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
    // CycleTick forfeits the lapsed called group and deletes both bodies.
    let nod = NodContract::new(storage.clone());
    nod.group_called_at.write(&group_key, 1)?;
    let notice = nod.group_call_notice_period_seconds.read(&group_key)?;
    outbe_primitives::expiry_queue::push(
        &outbe_nod::state::ExpiryHours(&nod),
        group_key,
        outbe_nod::api::settlement_deadline_of(1, notice),
    )?;
    seed_nod_daily_trigger(storage, fixture)?;

    Ok(())
}

fn seed_nod_daily_trigger(
    storage: StorageHandle<'_>,
    _fixture: &NodBodyFixture,
) -> eyre::Result<()> {
    let (.., pair_index) = outbe_oracle::api::require_coen_pair(storage.clone(), 840)?;
    let previous_day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(TEST_BLOCK_TIMESTAMP_BASE + 2),
    );
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    oracle.record_utc_day_vwap(previous_day, pair_index, U256::from(1_000_000u64))?;
    oracle.utc_day_vwap_last_finalized.write(previous_day)?;
    outbe_cycle::schema::Cycle::new(storage.clone())
        .last_executed_at
        .write(
            &outbe_cycle::triggers::TriggerId::NodCallDaily.as_u32(),
            TEST_BLOCK_TIMESTAMP_BASE - SECONDS_PER_DAY,
        )?;

    Ok(())
}
