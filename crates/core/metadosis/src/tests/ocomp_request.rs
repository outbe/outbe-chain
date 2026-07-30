use alloy_primitives::{address, b256, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_compressed_entities::{begin_block, end_block, ExecutionScope};
use outbe_desis::{AuctionStage, DesisContract};
use outbe_nod::NodContract;
use outbe_ocomp_protocol::state::{OcompJobStatus, OcompTerminalOutcome};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, METADOSIS_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    chain,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_tribute::{TributeContract, TributeData};

use super::{ocomp_storage::request_profile, TestParent};
use crate::{
    constants::{
        FORMING_PERIOD_HOURS, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS, SECONDS_PER_HOUR,
        WAITING_PERIOD_HOURS,
    },
    ocomp::{
        expiry::run_lifecycle_begin,
        request::run_terminal_request,
        schema::poc_schema_limits,
        state::{DayPhase, JobFsmLimits},
    },
    precompile::IMetadosis,
    schema::{day_type, status, MetadosisContract, WorldwideDayEntryExt},
};

#[test]
fn terminal_request_and_exclusive_expiry_commit_real_effects_atomically() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let scope = ExecutionScope::new();
    let parent = TestParent::empty();
    let wwd = outbe_common::WorldwideDay::new(2026_0708);
    let block_number = 19;
    let block_time = wwd.start_timestamp() + 8 * SECONDS_PER_HOUR;
    let owner = address!("7100000000000000000000000000000000000071");
    let nominal = U256::from(1_000);
    let day_limit = U256::from(100);
    let mut profile = request_profile();
    profile.chain_id = chain::CHAIN_ID;

    StorageHandle::enter(&mut provider, |storage| {
        seed_ce_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair("COEN", "0xUSD").unwrap();
        outbe_oracle::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .initialize_ocomp_request_profile(&profile, &poc_schema_limits())
            .unwrap();
        metadosis
            .create_worldwide_day(
                wwd,
                wwd.start_timestamp(),
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        let scheduled = wwd.start_timestamp()
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
            + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
            + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
        assert_eq!(
            metadosis.update_wwd_status(wwd, scheduled).unwrap(),
            status::READY
        );
        metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
        metadosis.set_wwd_vwap(wwd, U256::from(2)).unwrap();
        metadosis.set_metadosis_limit(wwd, day_limit).unwrap();
        metadosis.initialize_ocomp_pre_admission(wwd).unwrap();
        metadosis
            .enqueue_ocomp_ready(
                wwd,
                block_number,
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap();

        let mut tribute = TributeContract::new(storage.clone());
        tribute.initialize_fresh_ocomp_profile().unwrap();
        tribute.unseal_day(wwd).unwrap();
        tribute
            .issue(
                &scope,
                &parent,
                &TributeData {
                    tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                    owner,
                    worldwide_day: wwd,
                    issuance_amount_minor: nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::from(2),
                },
            )
            .unwrap();
        tribute.seal_day(wwd).unwrap();

        // Mirror production: the league snapshot is built in the active CE phase
        // (process_ocomp_ready_candidate) before the post-seal terminal request.
        metadosis
            .build_fidelity_league_snapshot(&scope, &parent, wwd, wwd.start_timestamp())
            .unwrap();

        end_block(storage.clone(), &scope).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, block_time, chain::CHAIN_ID),
            storage.clone(),
        );
        run_terminal_request(&ctx, &scope).unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        let fsm = metadosis
            .ocomp_fsm_state(
                wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap();
        let requested = fsm.projection();
        assert_eq!(requested.phase, DayPhase::OffchainPending);
        assert_eq!(requested.pending_nonce, 0);
        assert_eq!(requested.deadline_height, None);
        let intent_id = requested.live_intent_id.unwrap();
        let mut record = metadosis
            .ocomp_job_record(intent_id, &poc_schema_limits())
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::AwaitingFinality);
        assert_eq!(
            record.intent.ce_sealed_root,
            scope.completed_sealed_root().unwrap()
        );
        assert_eq!(record.intent.authenticated_day_count, 1);
        assert_eq!(record.intent.authenticated_day_nominal, nominal);
        let tribute_target = TributeContract::new(storage.clone())
            .pre_admission_projection(wwd)
            .unwrap();
        let nod_target = NodContract::new(storage.clone())
            .ocomp_target_projection(wwd)
            .unwrap();
        let contributor_target =
            outbe_intex::api::ocomp_contributor_target_projection(&storage, wwd.value()).unwrap();
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .tribute
                .source_generation,
            tribute_target.source_generation
        );
        assert_eq!(
            record.intent.activation_preconditions.nod.target_generation,
            nod_target.target_generation
        );
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .nod
                .namespace_root_before,
            nod_target.namespace_root_before
        );
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .contributors
                .expected_series_version,
            contributor_target.expected_series_version
        );
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            TributeContract::new(storage.clone())
                .total_supply()
                .unwrap(),
            1
        );
        assert_eq!(
            IMetadosis::getOffchainJobCall::SELECTOR,
            [0x4c, 0x13, 0x2d, 0x3d]
        );
        let public_call = IMetadosis::getOffchainJobCall {
            intentId: intent_id,
        };
        let encoded = crate::precompile::dispatch(
            storage.clone(),
            &public_call.abi_encode(),
            owner,
            U256::ZERO,
        )
        .unwrap();
        let public_record = IMetadosis::getOffchainJobCall::abi_decode_returns(&encoded).unwrap();
        assert_eq!(
            outbe_ocomp_protocol::state::OcompJobRecordV1::decode_canonical(
                public_record.as_ref(),
                &poc_schema_limits(),
            )
            .unwrap(),
            record
        );
        assert_eq!(
            DesisContract::new(storage.clone())
                .auction_stage
                .read(&wwd.value())
                .unwrap(),
            AuctionStage::Briefed as u8
        );

        let receipt_before = metadosis
            .request_budget_receipt(wwd, &poc_schema_limits())
            .unwrap()
            .unwrap();
        let desis_supply_before = DesisContract::new(storage.clone())
            .pending_supply_promis
            .read(&wwd.value())
            .unwrap();
        let finality_recorded_height = block_number + 2;
        let finalized = MetadosisContract::new(storage.clone())
            .record_ocomp_finality(
                intent_id,
                B256::repeat_byte(0x46),
                B256::repeat_byte(0x98),
                finality_recorded_height,
                profile.capacity_profile.result_deadline_blocks,
                &poc_schema_limits(),
            )
            .unwrap();
        let open = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(finalized.open_height, block_time + 6, chain::CHAIN_ID),
            storage.clone(),
        );
        run_lifecycle_begin(&open).unwrap();
        record = MetadosisContract::new(storage.clone())
            .ocomp_job_record(intent_id, &poc_schema_limits())
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::VotingOpen);
        let expiry_height = finalized.deadline_height;
        let expiry = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(expiry_height, block_time + 64, chain::CHAIN_ID),
            storage.clone(),
        );
        run_lifecycle_begin(&expiry).unwrap();
        run_terminal_request(&expiry, &scope).unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        let after = metadosis
            .ocomp_fsm_state(
                wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();
        assert_eq!(after.phase, DayPhase::Ready);
        assert_eq!(after.pending_nonce, 1);
        assert_eq!(after.next_check_height, Some(expiry_height + 1));
        assert_eq!(
            metadosis
                .request_budget_receipt(wwd, &poc_schema_limits())
                .unwrap(),
            Some(receipt_before.clone())
        );
        assert_eq!(
            DesisContract::new(storage.clone())
                .pending_supply_promis
                .read(&wwd.value())
                .unwrap(),
            desis_supply_before
        );
        let terminal = metadosis
            .ocomp_job_record(intent_id, &poc_schema_limits())
            .unwrap()
            .unwrap();
        assert_eq!(terminal.status, OcompJobStatus::Expired);
        assert_eq!(
            terminal.terminal.as_ref().unwrap().outcome,
            OcompTerminalOutcome::Expired
        );
        assert_eq!(
            terminal.terminal.as_ref().unwrap().next_pending_nonce,
            Some(1)
        );

        let retry_height = expiry_height + 1;
        let retry = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(retry_height, block_time + 65, chain::CHAIN_ID),
            storage.clone(),
        );
        run_lifecycle_begin(&retry).unwrap();
        run_terminal_request(&retry, &scope).unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        let retried = metadosis
            .ocomp_fsm_state(
                wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();
        assert_eq!(retried.phase, DayPhase::OffchainPending);
        assert_eq!(retried.pending_nonce, 1);
        assert_eq!(retried.deadline_height, None);
        let retry_intent_id = retried.live_intent_id.unwrap();
        assert_ne!(retry_intent_id, intent_id);
        let retried_record = metadosis
            .ocomp_job_record(retry_intent_id, &poc_schema_limits())
            .unwrap()
            .unwrap();
        assert_eq!(retried_record.status, OcompJobStatus::AwaitingFinality);
        assert_eq!(retried_record.intent.pending_nonce, 1);
        assert_eq!(retried_record.intent.attempt, 1);
        assert_eq!(
            retried_record
                .intent
                .frozen_metadosis_values
                .request_budget_split_receipt_hash,
            record
                .intent
                .frozen_metadosis_values
                .request_budget_split_receipt_hash
        );
        assert_eq!(
            metadosis
                .request_budget_receipt(wwd, &poc_schema_limits())
                .unwrap(),
            Some(receipt_before)
        );
        assert_eq!(
            DesisContract::new(storage.clone())
                .pending_supply_promis
                .read(&wwd.value())
                .unwrap(),
            desis_supply_before
        );
        assert_eq!(NodContract::new(storage.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            TributeContract::new(storage.clone())
                .total_supply()
                .unwrap(),
            1
        );
    });

    let logs = provider.get_ordered_events();
    assert_eq!(
        IMetadosis::OffchainJobRequested::SIGNATURE_HASH,
        b256!("69a11b11a3b39ad0968d02a67ee3b9e2d790cb9aafbc4de957beed93c39b7dad")
    );
    assert_eq!(
        IMetadosis::OffchainJobExpired::SIGNATURE_HASH,
        b256!("9ca54d8b0ae876fcd3b6e519643b9409e3694c8110d3e17f72f8baa51cea320d")
    );
    let requested = logs
        .iter()
        .find_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
        .expect("OffchainJobRequested event");
    assert_eq!(requested.data.wwd, wwd.value());
    assert_eq!(requested.data.pendingNonce, 0);
    let expired = logs
        .iter()
        .find_map(|log| IMetadosis::OffchainJobExpired::decode_log(log).ok())
        .expect("OffchainJobExpired event");
    assert_eq!(expired.data.intentId, requested.data.intentId);
    assert_eq!(expired.data.nextPendingNonce, 1);
    let requests = logs
        .iter()
        .filter_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].data.pendingNonce, 0);
    assert_eq!(requests[0].data.attempt, 0);
    assert_eq!(requests[1].data.pendingNonce, 1);
    assert_eq!(requests[1].data.attempt, 1);
    assert_ne!(requests[0].data.intentId, requests[1].data.intentId);
    assert!(logs.iter().all(|log| log.address != METADOSIS_ADDRESS
        || log.data.topics().first() == Some(&IMetadosis::OffchainJobRequested::SIGNATURE_HASH)
        || log.data.topics().first() == Some(&IMetadosis::OffchainJobExpired::SIGNATURE_HASH)));
}

#[test]
fn ineligible_request_defers_only_the_ready_key_without_effects() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, false);

    StorageHandle::enter(&mut provider, |storage| {
        let before = request_observables(storage.clone(), fixture.wwd);
        assert_eq!(before.fsm.phase, DayPhase::Ready);
        assert_eq!(before.fsm.next_check_height, Some(fixture.block_number));

        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                fixture.block_number,
                fixture.block_time,
                chain::CHAIN_ID,
            ),
            storage.clone(),
        );
        run_terminal_request(&ctx, &fixture.scope).unwrap();

        let after = request_observables(storage, fixture.wwd);
        assert_eq!(after.fsm.phase, DayPhase::Ready);
        assert_eq!(after.fsm.pending_nonce, 0);
        assert_eq!(after.fsm.next_check_height, Some(fixture.block_number + 1));
        assert_eq!(after.fsm.terminal_records, 0);
        assert!(after.fsm.live_intent_id.is_none());
        assert_eq!(after.receipt, None);
        assert_eq!(after.desis_stage, AuctionStage::None as u8);
        assert_eq!(after.desis_supply, U256::ZERO);
        assert_eq!(after.nod_supply, 0);
        assert_eq!(after.tribute_supply, 1);
        assert!(!after.tribute_pre_admission.is_sealed);
    });

    assert!(provider
        .get_ordered_events()
        .iter()
        .all(|log| IMetadosis::OffchainJobRequested::decode_log(log).is_err()));
}

#[test]
fn deferred_day_does_not_starve_a_later_eligible_job_intent() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    // Oracle starts un-armed so the first ready day defers (OracleProfileNotReady);
    // it is armed mid-test so the later day becomes eligible.
    let fixture = prepare_two_ready_days_fixture(&mut provider, false);

    StorageHandle::enter(&mut provider, |storage| {
        let first_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                fixture.block_number,
                fixture.block_time,
                chain::CHAIN_ID,
            ),
            storage.clone(),
        );
        run_terminal_request(&first_ctx, &fixture.scope).unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        let first = metadosis
            .ocomp_fsm_state(
                fixture.first_wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();
        assert_eq!(first.phase, DayPhase::Ready);
        assert_eq!(first.next_check_height, Some(fixture.block_number + 1));
        assert_eq!(
            metadosis
                .next_ocomp_ready(
                    &poc_schema_limits(),
                    JobFsmLimits {
                        max_terminal_records: 365,
                    },
                )
                .unwrap()
                .unwrap()
                .worldwide_day,
            fixture.later_wwd
        );

        // Arm Oracle so the later day is eligible on the next block; the first
        // day's deferral must not have starved it.
        outbe_oracle::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();

        let next_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                fixture.block_number + 1,
                fixture.block_time + 1,
                chain::CHAIN_ID,
            ),
            storage.clone(),
        );
        run_terminal_request(&next_ctx, &fixture.scope).unwrap();

        let metadosis = MetadosisContract::new(storage);
        let later = metadosis
            .ocomp_fsm_state(
                fixture.later_wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();
        assert_eq!(later.phase, DayPhase::OffchainPending);
        let record = metadosis
            .ocomp_job_record(later.live_intent_id.unwrap(), &poc_schema_limits())
            .unwrap()
            .unwrap();
        assert_eq!(record.intent.wwd, fixture.later_wwd.value());
        assert_eq!(record.status, OcompJobStatus::AwaitingFinality);
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(fixture.first_wwd)
                .status()
                .read()
                .unwrap(),
            status::READY
        );
    });

    let requested = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| IMetadosis::OffchainJobRequested::decode_log(log).ok())
        .collect::<Vec<_>>();
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[0].data.wwd, fixture.later_wwd.value());
}

#[test]
fn two_eligible_days_create_independently_progressing_live_jobs() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_two_ready_days_fixture(&mut provider, true);

    StorageHandle::enter(&mut provider, |storage| {
        for (block_number, block_time) in [
            (fixture.block_number, fixture.block_time),
            (fixture.block_number + 1, fixture.block_time + 1),
        ] {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(block_number, block_time, chain::CHAIN_ID),
                storage.clone(),
            );
            run_terminal_request(&ctx, &fixture.scope).unwrap();
        }

        let mut metadosis = MetadosisContract::new(storage);
        let first = metadosis
            .ocomp_fsm_state(
                fixture.first_wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();
        let second = metadosis
            .ocomp_fsm_state(
                fixture.later_wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();

        assert_eq!(first.phase, DayPhase::OffchainPending);
        assert_eq!(second.phase, DayPhase::OffchainPending);
        assert_ne!(first.live_intent_id, second.live_intent_id);
        let first_intent_id = first.live_intent_id.unwrap();
        let second_intent_id = second.live_intent_id.unwrap();
        for projection in [first, second] {
            let record = metadosis
                .ocomp_job_record(projection.live_intent_id.unwrap(), &poc_schema_limits())
                .unwrap()
                .unwrap();
            assert_eq!(record.status, OcompJobStatus::AwaitingFinality);
        }

        let finalized = metadosis
            .record_ocomp_finality(
                first_intent_id,
                B256::repeat_byte(0x81),
                B256::repeat_byte(0x82),
                fixture.block_number + 2,
                request_profile().capacity_profile.result_deadline_blocks,
                &poc_schema_limits(),
            )
            .unwrap();
        assert!(metadosis
            .open_due_ocomp_voting(
                finalized.open_height,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap());
        metadosis
            .expire_ocomp_job(
                first_intent_id,
                finalized.deadline_height,
                fixture.block_time + 64,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap();

        assert_eq!(
            metadosis
                .ocomp_fsm_state(
                    fixture.first_wwd,
                    &poc_schema_limits(),
                    JobFsmLimits {
                        max_terminal_records: 365,
                    },
                )
                .unwrap()
                .projection()
                .phase,
            DayPhase::Ready
        );
        let survivor = metadosis
            .live_ocomp_fsm_state_by_intent(
                second_intent_id,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .unwrap()
            .projection();
        assert_eq!(survivor.phase, DayPhase::OffchainPending);
        assert_eq!(survivor.live_intent_id, Some(second_intent_id));
    });
}

#[test]
fn nonzero_owner_projections_are_snapshotted_in_the_created_intent() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);

    StorageHandle::enter(&mut provider, |storage| {
        let tribute = TributeContract::new(storage.clone());
        let mut admission = tribute.day_pre_admission.get(fixture.wwd).unwrap().unwrap();
        admission.source_generation = 3;
        tribute.day_pre_admission.update(&admission).unwrap();

        let nod = NodContract::new(storage.clone());
        nod.ocomp_target_generation.write(&fixture.wwd, 7).unwrap();
        nod.ocomp_namespace_root
            .write(&fixture.wwd, B256::repeat_byte(0x77))
            .unwrap();
        nod.ocomp_bucket_root
            .write(&fixture.wwd, B256::repeat_byte(0x78))
            .unwrap();
        nod.ocomp_output_manifest_root
            .write(&fixture.wwd, B256::repeat_byte(0x79))
            .unwrap();
        nod.ocomp_generation_metadata
            .write(
                &fixture.wwd,
                U256::from(1_u64)
                    | (U256::from(1_u32) << 64)
                    | (U256::from(1_u32) << 96)
                    | (U256::from(1_u32) << 128),
            )
            .unwrap();

        outbe_intex::api::create_series(
            &storage,
            outbe_intex::CreateSeriesParams {
                series_id: fixture.wwd.value(),
                worldwide_day: fixture.wwd.value(),
                issued_intex_count: 1,
                promis_load_minor: 1,
                entry_price_minor: U256::from(1),
                floor_price_minor: U256::from(1),
                call_price_minor: U256::from(1),
                call_trigger: outbe_intex::IntexCallTrigger {
                    window_days: 1,
                    threshold_days: 1,
                    intex_call_period: 1,
                },
                issued_at: 1,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();

        let expected_tribute = tribute.pre_admission_projection(fixture.wwd).unwrap();
        let expected_nod = nod.ocomp_target_projection(fixture.wwd).unwrap();
        let expected_contributors =
            outbe_intex::api::ocomp_contributor_target_projection(&storage, fixture.wwd.value())
                .unwrap();
        assert_ne!(expected_tribute.source_generation, 0);
        assert_ne!(expected_nod.target_generation, 0);
        assert!(!expected_nod.namespace_root_before.is_zero());
        assert_ne!(expected_contributors.expected_series_version, 0);

        let before_nod_supply = nod.total_supply().unwrap();
        let before_tribute_supply = tribute.total_supply().unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                fixture.block_number,
                fixture.block_time,
                chain::CHAIN_ID,
            ),
            storage.clone(),
        );
        run_terminal_request(&ctx, &fixture.scope).unwrap();

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_wwd_status(fixture.wwd).unwrap(),
            status::OFFCHAIN_PENDING
        );
        let live = metadosis
            .ocomp_fsm_state(
                fixture.wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection();
        let intent_id = live.live_intent_id.unwrap();
        let record = metadosis
            .ocomp_job_record(intent_id, &poc_schema_limits())
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::AwaitingFinality);
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .tribute
                .source_generation,
            expected_tribute.source_generation
        );
        assert_eq!(
            record.intent.activation_preconditions.nod.target_generation,
            expected_nod.target_generation
        );
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .nod
                .namespace_root_before,
            expected_nod.namespace_root_before
        );
        assert_eq!(
            record
                .intent
                .activation_preconditions
                .contributors
                .expected_series_version,
            expected_contributors.expected_series_version
        );
        assert!(metadosis
            .request_budget_receipt(fixture.wwd, &poc_schema_limits())
            .unwrap()
            .is_some());
        assert_eq!(nod.total_supply().unwrap(), before_nod_supply);
        assert_eq!(tribute.total_supply().unwrap(), before_tribute_supply);
    });

    assert_eq!(
        provider
            .get_ordered_events()
            .iter()
            .filter(|log| IMetadosis::OffchainJobRequested::decode_log(log).is_ok())
            .count(),
        1
    );
}

#[test]
fn request_storage_failure_rolls_back_every_observable_effect() {
    let mut calibration = HashMapStorageProvider::new(chain::CHAIN_ID);
    let calibration_fixture = prepare_request_fixture(&mut calibration, true);
    calibration.fail_mutation_at(usize::MAX);
    StorageHandle::enter(&mut calibration, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                calibration_fixture.block_number,
                calibration_fixture.block_time,
                chain::CHAIN_ID,
            ),
            storage,
        );
        run_terminal_request(&ctx, &calibration_fixture.scope).unwrap();
    });
    let successful_mutations = calibration.clear_mutation_failure();
    assert!(
        successful_mutations >= 8,
        "fixture must cross several independently owned request writes"
    );

    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    let before = StorageHandle::enter(&mut provider, |storage| {
        request_observables(storage, fixture.wwd)
    });
    let events_before = provider.get_ordered_events().len();
    let failure_operation = successful_mutations / 2;
    provider.fail_after_mutation_at(failure_operation);

    let error = StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                fixture.block_number,
                fixture.block_time,
                chain::CHAIN_ID,
            ),
            storage,
        );
        run_terminal_request(&ctx, &fixture.scope).unwrap_err()
    });
    assert!(matches!(
        error,
        outbe_primitives::error::PrecompileError::Storage(_)
    ));
    assert!(provider.clear_mutation_failure() > failure_operation);

    let after = StorageHandle::enter(&mut provider, |storage| {
        request_observables(storage, fixture.wwd)
    });
    assert_eq!(after, before);
    assert_eq!(provider.get_ordered_events().len(), events_before);
    assert!(provider
        .get_ordered_events()
        .iter()
        .all(|log| IMetadosis::OffchainJobRequested::decode_log(log).is_err()));
}

fn seed_ce_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
}

struct PreparedRequestFixture {
    scope: ExecutionScope,
    wwd: outbe_common::WorldwideDay,
    block_number: u64,
    block_time: u64,
}

struct TwoReadyDaysFixture {
    scope: ExecutionScope,
    first_wwd: outbe_common::WorldwideDay,
    later_wwd: outbe_common::WorldwideDay,
    block_number: u64,
    block_time: u64,
}

fn prepare_request_fixture(
    provider: &mut HashMapStorageProvider,
    oracle_ready: bool,
) -> PreparedRequestFixture {
    let scope = ExecutionScope::new();
    let parent = TestParent::empty();
    let wwd = outbe_common::WorldwideDay::new(2026_0709);
    let block_number = 23;
    let block_time = wwd.start_timestamp() + 8 * SECONDS_PER_HOUR;
    let owner = address!("7200000000000000000000000000000000000072");
    let nominal = U256::from(1_000);
    let mut profile = request_profile();
    profile.chain_id = chain::CHAIN_ID;

    StorageHandle::enter(provider, |storage| {
        seed_ce_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();

        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair("COEN", "0xUSD").unwrap();
        // `oracle_ready` gates OCOMP admission: when false the Oracle profile is
        // left un-armed so the terminal request defers with OracleProfileNotReady
        // (the deferral path formerly exercised via Fidelity readiness).
        if oracle_ready {
            outbe_oracle::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();
        }

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .initialize_ocomp_request_profile(&profile, &poc_schema_limits())
            .unwrap();
        metadosis
            .create_worldwide_day(
                wwd,
                wwd.start_timestamp(),
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        let scheduled = wwd.start_timestamp()
            + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
            + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
            + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
            + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
        assert_eq!(
            metadosis.update_wwd_status(wwd, scheduled).unwrap(),
            status::READY
        );
        metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
        metadosis.set_wwd_vwap(wwd, U256::from(2)).unwrap();
        metadosis.set_metadosis_limit(wwd, U256::from(100)).unwrap();
        metadosis.initialize_ocomp_pre_admission(wwd).unwrap();
        metadosis
            .enqueue_ocomp_ready(
                wwd,
                block_number,
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap();

        let mut tribute = TributeContract::new(storage.clone());
        tribute.initialize_fresh_ocomp_profile().unwrap();
        tribute.unseal_day(wwd).unwrap();
        tribute
            .issue(
                &scope,
                &parent,
                &TributeData {
                    tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                    owner,
                    worldwide_day: wwd,
                    issuance_amount_minor: nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::from(2),
                },
            )
            .unwrap();
        tribute.seal_day(wwd).unwrap();
        // Mirror production: the league snapshot is built in the active CE phase
        // before the post-seal terminal request reads its committed root.
        metadosis
            .build_fidelity_league_snapshot(&scope, &parent, wwd, wwd.start_timestamp())
            .unwrap();
        end_block(storage, &scope).unwrap();
    });

    PreparedRequestFixture {
        scope,
        wwd,
        block_number,
        block_time,
    }
}

fn prepare_two_ready_days_fixture(
    provider: &mut HashMapStorageProvider,
    oracle_ready: bool,
) -> TwoReadyDaysFixture {
    let scope = ExecutionScope::new();
    let parent = TestParent::empty();
    let first_wwd = outbe_common::WorldwideDay::new(2026_0710);
    let later_wwd = outbe_common::WorldwideDay::new(2026_0711);
    let block_number = 29;
    let block_time = later_wwd.start_timestamp() + 8 * SECONDS_PER_HOUR;
    let mut profile = request_profile();
    profile.chain_id = chain::CHAIN_ID;

    StorageHandle::enter(provider, |storage| {
        seed_ce_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair("COEN", "0xUSD").unwrap();
        // When false the Oracle profile is left un-armed so the terminal request
        // defers with OracleProfileNotReady (arm it mid-test to make a day eligible).
        if oracle_ready {
            outbe_oracle::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();
        }

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .initialize_ocomp_request_profile(&profile, &poc_schema_limits())
            .unwrap();
        for wwd in [first_wwd, later_wwd] {
            metadosis
                .create_worldwide_day(
                    wwd,
                    wwd.start_timestamp(),
                    LOOKBACK_DELAY_HOURS,
                    OFFERING_PERIOD_HOURS,
                )
                .unwrap();
            let scheduled = wwd.start_timestamp()
                + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR
                + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR
                + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR
                + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
            assert_eq!(
                metadosis.update_wwd_status(wwd, scheduled).unwrap(),
                status::READY
            );
            metadosis.set_wwd_day_type(wwd, day_type::GREEN).unwrap();
            metadosis.set_wwd_vwap(wwd, U256::from(2)).unwrap();
            metadosis.set_metadosis_limit(wwd, U256::from(100)).unwrap();
            metadosis.initialize_ocomp_pre_admission(wwd).unwrap();
            metadosis
                .enqueue_ocomp_ready(
                    wwd,
                    block_number,
                    JobFsmLimits {
                        max_terminal_records: 365,
                    },
                )
                .unwrap();
        }

        let mut tribute = TributeContract::new(storage.clone());
        tribute.initialize_fresh_ocomp_profile().unwrap();
        for (ordinal, wwd) in [first_wwd, later_wwd].into_iter().enumerate() {
            tribute.unseal_day(wwd).unwrap();
            let owner = if ordinal == 0 {
                address!("7300000000000000000000000000000000000073")
            } else {
                address!("7400000000000000000000000000000000000074")
            };
            tribute
                .issue(
                    &scope,
                    &parent,
                    &TributeData {
                        tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                        owner,
                        worldwide_day: wwd,
                        issuance_amount_minor: U256::from(1_000),
                        issuance_currency: 840,
                        nominal_amount_minor: U256::from(1_000),
                        reference_currency: 840,
                        exclude_from_intex_issuance: false,
                        tribute_price_minor: U256::from(2),
                    },
                )
                .unwrap();
            tribute.seal_day(wwd).unwrap();
            // Mirror production: build each day's league snapshot in the active
            // CE phase before the post-seal terminal request.
            metadosis
                .build_fidelity_league_snapshot(&scope, &parent, wwd, wwd.start_timestamp())
                .unwrap();
        }
        end_block(storage, &scope).unwrap();
    });

    TwoReadyDaysFixture {
        scope,
        first_wwd,
        later_wwd,
        block_number,
        block_time,
    }
}

#[derive(Debug, PartialEq)]
struct RequestObservables {
    fsm: crate::ocomp::state::JobFsmProjection,
    receipt: Option<outbe_ocomp_protocol::receipts::RequestBudgetSplitReceiptV1>,
    desis_stage: u8,
    desis_supply: U256,
    nod_supply: u64,
    tribute_supply: u64,
    tribute_pre_admission: outbe_tribute::TributePreAdmissionProjection,
}

fn request_observables(
    storage: StorageHandle<'_>,
    wwd: outbe_common::WorldwideDay,
) -> RequestObservables {
    RequestObservables {
        fsm: MetadosisContract::new(storage.clone())
            .ocomp_fsm_state(
                wwd,
                &poc_schema_limits(),
                JobFsmLimits {
                    max_terminal_records: 365,
                },
            )
            .unwrap()
            .projection(),
        receipt: MetadosisContract::new(storage.clone())
            .request_budget_receipt(wwd, &poc_schema_limits())
            .unwrap(),
        desis_stage: DesisContract::new(storage.clone())
            .auction_stage
            .read(&wwd.value())
            .unwrap(),
        desis_supply: DesisContract::new(storage.clone())
            .pending_supply_promis
            .read(&wwd.value())
            .unwrap(),
        nod_supply: NodContract::new(storage.clone()).total_supply().unwrap(),
        tribute_supply: TributeContract::new(storage.clone())
            .total_supply()
            .unwrap(),
        tribute_pre_admission: TributeContract::new(storage)
            .pre_admission_projection(wwd)
            .unwrap(),
    }
}
