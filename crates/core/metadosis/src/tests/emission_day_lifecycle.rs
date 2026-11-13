use super::*;
use crate::api;
use alloy_sol_types::SolEvent;
use outbe_system_common::time::{date_key_to_utc_timestamp, next_date_key, WorldwideDay};

fn advance_at(storage: &StorageHandle<'_>, timestamp: u64, genesis_timestamp: u64) {
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
    let block = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: storage.block_number().unwrap(),
        timestamp,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp,
        proposer: alloy_primitives::Address::ZERO,
        validators: Vec::new(),
    };
    let ctx = BlockRuntimeContext::new(block, storage.clone());
    outbe_tributefactory::worldwideday::advance_worldwide_days_command(&ctx).unwrap();
}

#[test]
fn init_genesis_day_creates_7_day_lookahead_with_bidirectional_links_and_closed_form_limits() {
    with_storage(|storage| {
        let genesis_ts = date_key_to_utc_timestamp(20260920).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);

        let opened = TributeFactoryContract::new(storage.clone())
            .pipeline_wwd
            .read_all()
            .unwrap();
        assert_eq!(opened.len(), 2);
        assert!(opened.contains(&WorldwideDay::new(20260920)));
        assert!(opened.contains(&WorldwideDay::new(20260921)));

        for day in [20260921, 20260922, 20260923, 20260924, 20260925] {
            advance_at(
                &storage,
                date_key_to_utc_timestamp(day).unwrap(),
                genesis_ts,
            );
        }

        let pipeline = TributeFactoryContract::new(storage.clone())
            .pipeline_wwd
            .read_all()
            .unwrap();
        assert_eq!(pipeline.len(), 6);
        assert!(!pipeline.contains(&WorldwideDay::new(20260920)));
        assert!(pipeline.contains(&WorldwideDay::new(20260926)));

        // Verify wwd = 20260920 -> day = 20260924 (and wwd = 20260925 -> day = 20260929)
        let wwd_20 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260920))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_20.day, 20260924);

        let ed_24 = api::emission_day(storage.clone(), 20260924)
            .unwrap()
            .unwrap();
        assert_eq!(ed_24.day, 20260924);
        assert_eq!(ed_24.wwd, WorldwideDay::new(20260920));
        assert_eq!(
            ed_24.metadosis_start,
            date_key_to_utc_timestamp(next_date_key(20260924).expect("date step")).unwrap() + 1
        );
        assert_eq!(ed_24.state, metadosis_state::PENDING);
        assert_eq!(ed_24.emission_limit_minor, U256::ZERO);

        let wwd_25 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260925))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_25.day, 20260929);
        let ed_29 = api::emission_day(storage.clone(), 20260929)
            .unwrap()
            .unwrap();
        assert_eq!(ed_29.wwd, WorldwideDay::new(20260925));
        assert_eq!(ed_29.emission_limit_minor, U256::ZERO);

        // 20260920..=20260926 stay linked after the daily steps. 20260920 is sealed
        // on 20260925 and leaves the pipeline; the record and its emission day remain.
        let mut wwd_key = 20260920;
        for _ in 0..7u32 {
            let wwd = WorldwideDay::new(wwd_key);
            let wwd_proj = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
            let ed_proj = api::emission_day(storage.clone(), wwd_proj.day)
                .unwrap()
                .unwrap();
            assert_eq!(ed_proj.wwd, wwd);
            assert_eq!(ed_proj.emission_limit_minor, U256::ZERO);
            assert_eq!(
                ed_proj.metadosis_start,
                date_key_to_utc_timestamp(next_date_key(ed_proj.day).expect("date step")).unwrap()
                    + 1
            );
            wwd_key = next_date_key(wwd_key).expect("date step");
        }
    });
}

#[test]
fn schedule_places_emission_day_after_offering_wwd_and_resumes_oldest_due_after_halt() {
    with_storage(|storage| {
        let wwd_sep_01 = WorldwideDay::new(20260901);
        let genesis_ts = date_key_to_utc_timestamp(20260901).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: genesis_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: genesis_ts,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );

        let wwd_proj = api::worldwide_day(storage.clone(), wwd_sep_01)
            .unwrap()
            .unwrap();
        assert_eq!(
            WorldwideDay::from_timestamp(wwd_proj.offerings_start),
            WorldwideDay::new(20260904)
        );
        assert_eq!(
            wwd_proj.offering_end,
            date_key_to_utc_timestamp(20260905).unwrap() + 12 * 3_600
        );
        assert_eq!(wwd_proj.day, 20260905);

        let expected_metadosis_start = date_key_to_utc_timestamp(20260906).unwrap() + 1;
        assert_eq!(
            expected_metadosis_start,
            wwd_proj.offering_end + 12 * 3_600 + 1
        );

        let ed_05 = api::emission_day(storage.clone(), 20260905)
            .unwrap()
            .unwrap();
        assert_eq!(ed_05.day, 20260905);
        assert_eq!(ed_05.wwd, wwd_sep_01);
        assert_eq!(ed_05.metadosis_start, expected_metadosis_start);
        assert_eq!(ed_05.emission_limit_minor, U256::ZERO);

        // Nested process-ready checks recompute the lease from the provider clock
        // and block. Allocate day 20260905 into Processing.
        allocate_to_metadosis(&ctx, 20260905, U256::from(1_000_000u64));

        let at_start = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: expected_metadosis_start,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: genesis_ts,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        storage
            .set_block_timestamp(U256::from(at_start.block.timestamp))
            .unwrap();
        outbe_tributefactory::worldwideday::advance_worldwide_days_command(&at_start).unwrap();

        // Simulate a halt from 5 Sep to 8 Sep 2026: the first block after restart
        // still runs the Metadosis of 20260905.
        let restart_ts = date_key_to_utc_timestamp(20260908).unwrap() + 3_600;
        arm_reference_price(&storage, restart_ts);
        storage.set_block_timestamp(U256::from(restart_ts)).unwrap();
        let aligned = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: restart_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(restart_ts, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        crate::commands::process_emission_day(&aligned, 20260905).unwrap();
        let completed = api::emission_day(storage.clone(), 20260905)
            .unwrap()
            .unwrap();
        assert_eq!(completed.day, 20260905);
        assert_eq!(completed.wwd, wwd_sep_01);
        assert_eq!(completed.state, metadosis_state::COMPLETED);
        let desis = outbe_desis::schema::DesisContract::new(storage);
        let briefed = outbe_system_common::time::EmissionDate::new(20260905);
        assert_eq!(
            desis.auction_stage.read(&briefed).unwrap(),
            outbe_desis::schema::AuctionStage::Briefed as u8
        );
        assert_eq!(desis.brief_has_limit.read(&briefed).unwrap(), 0);
    });
}

#[test]
fn first_wwd_for_genesis_splits_3_or_4_empty_emission_days_across_noon_utc() {
    // Genesis on 2026-10-01 at 11:59 UTC -> first_wwd = 20260930 (G - 1),
    // offering_end on 20261004 (G + 3) -> 3 empty EmissionDays (20261001..=20261003).
    with_storage(|storage| {
        let genesis_before_noon =
            date_key_to_utc_timestamp(20261001).unwrap() + 11 * 3_600 + 59 * 60;
        assert_eq!(
            outbe_tributefactory::worldwideday::first_wwd_for_genesis(genesis_before_noon),
            WorldwideDay::new(20260930)
        );
        storage
            .set_block_timestamp(U256::from(genesis_before_noon))
            .unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: genesis_before_noon,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(genesis_before_noon, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        outbe_tributefactory::worldwideday::advance_worldwide_days_command(&ctx).unwrap();

        let first_proj = api::worldwide_day(storage.clone(), WorldwideDay::new(20260930))
            .unwrap()
            .unwrap();
        assert_eq!(first_proj.day, 20261004);
        let mut day = 20261001;
        let mut empty_count = 0u32;
        while day < first_proj.day {
            let shell = outbe_emission::api::ensure_emission_day_shell(storage.clone(), day)
                .expect("empty day shell");
            assert_eq!(shell.wwd, WorldwideDay::new(0));
            empty_count += 1;
            day = next_date_key(day).unwrap();
        }
        assert_eq!(empty_count, 3);

        allocate_to_metadosis(&ctx, 20261001, U256::from(1_000_000u64));
        let empty_due_ts = date_key_to_utc_timestamp(20261002).unwrap() + 1;
        storage
            .set_block_timestamp(U256::from(empty_due_ts))
            .unwrap();
        let empty_due_ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: empty_due_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(empty_due_ts, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        crate::commands::process_emission_day(&empty_due_ctx, 20261001).unwrap();
        let settled_empty = api::emission_day(storage, 20261001).unwrap().unwrap();
        assert_eq!(settled_empty.wwd, WorldwideDay::new(0));
        assert_eq!(settled_empty.state, metadosis_state::COMPLETED);
    });

    // Genesis on 2026-10-01 at 12:01 UTC -> first_wwd = 20261001 (G),
    // offering_end on 20261005 (G + 4) -> 4 empty EmissionDays (20261001..=20261004).
    with_storage(|storage| {
        let genesis_after_noon = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600 + 60;
        assert_eq!(
            outbe_tributefactory::worldwideday::first_wwd_for_genesis(genesis_after_noon),
            WorldwideDay::new(20261001)
        );
        storage
            .set_block_timestamp(U256::from(genesis_after_noon))
            .unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: genesis_after_noon,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(genesis_after_noon, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        outbe_tributefactory::worldwideday::advance_worldwide_days_command(&ctx).unwrap();

        let first_proj = api::worldwide_day(storage.clone(), WorldwideDay::new(20261001))
            .unwrap()
            .unwrap();
        assert_eq!(first_proj.day, 20261005);
        let mut day = 20261001;
        let mut empty_count = 0u32;
        while day < first_proj.day {
            let shell = outbe_emission::api::ensure_emission_day_shell(storage.clone(), day)
                .expect("empty day shell");
            assert_eq!(shell.wwd, WorldwideDay::new(0));
            empty_count += 1;
            day = next_date_key(day).unwrap();
        }
        assert_eq!(empty_count, 4);

        // Catch-up on 2026-10-11 opens every WWD whose offering_end is still after
        // 2026-10-11 00:00 UTC: 20261007..=20261012.
        let catchup_ts = date_key_to_utc_timestamp(20261011).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(catchup_ts)).unwrap();
        let catchup_block = BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: storage.block_number().unwrap(),
            timestamp: catchup_ts,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: genesis_after_noon,
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        };
        let catchup_ctx = BlockRuntimeContext::new(catchup_block, storage.clone());
        outbe_tributefactory::worldwideday::advance_worldwide_days_command(&catchup_ctx).unwrap();
        for wwd_key in 20261007..=20261012 {
            let proj = api::worldwide_day(storage.clone(), WorldwideDay::new(wwd_key))
                .unwrap()
                .expect("catch-up and lookahead WWD must exist");
            let ed = api::emission_day(storage.clone(), proj.day)
                .unwrap()
                .expect("linked emission day must exist");
            assert_eq!(ed.wwd, WorldwideDay::new(wwd_key));
        }
    });
}

#[test]
fn wwd_lifecycle_gates_activation_on_forming_vwap_and_seals_on_close() {
    with_storage(|storage| {
        let wwd = WorldwideDay::new(20261001);
        let genesis_ts = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);

        let tribute = TributeContract::new(storage.clone());
        let factory = TributeFactoryContract::new(storage.clone());
        let proj = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(
            proj.forming_end,
            date_key_to_utc_timestamp(20261002).unwrap() + 12 * 3_600
        );
        assert_eq!(
            proj.offerings_start,
            date_key_to_utc_timestamp(20261003).unwrap() + 10 * 3_600
        );
        assert_eq!(
            proj.offering_end,
            date_key_to_utc_timestamp(20261005).unwrap() + 12 * 3_600
        );
        assert!(!tribute.is_day_sealed(wwd).unwrap());
        assert!(!api::is_offering_day(storage.clone(), wwd).unwrap());
        assert!(!factory.forming_snapshot_finalized.read(&wwd).unwrap());

        // The daily step on 2026-10-03 00:00 UTC snapshots forming. No oracle
        // prices are present, so the snapshot is finalized empty.
        let snapshot_ts = proj.forming_end + 12 * 3_600;
        advance_at(&storage, snapshot_ts, genesis_ts);
        assert!(factory.forming_snapshot_finalized.read(&wwd).unwrap());
        assert!(!outbe_oracle::api::has_worldwide_day_vwap_snapshot(storage.clone(), wwd).unwrap());

        // Same UTC day, at offerings_start: no stored forming price, so the day
        // takes no offers.
        storage
            .set_block_timestamp(U256::from(proj.offerings_start))
            .unwrap();
        assert!(!api::is_offering_day(storage.clone(), wwd).unwrap());
        assert!(!api::offering_worldwide_days(storage.clone())
            .unwrap()
            .contains(&wwd));

        // The daily step on 2026-10-06 00:00 UTC seals the offering window.
        let seal_ts = proj.offering_end + 12 * 3_600;
        advance_at(&storage, seal_ts, genesis_ts);
        assert!(!factory.pipeline_wwd.read_all().unwrap().contains(&wwd));
        assert!(tribute.is_day_sealed(wwd).unwrap());
    });
}

#[test]
fn wwd_without_price_runs_as_empty_day() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 64);
    install_test_enclaves();
    StorageHandle::enter(&mut provider, |storage| {
        let wwd = WorldwideDay::new(20261001);
        let emission_day = 20261005u32;
        let genesis_ts = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);

        let proj = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(proj.day, emission_day);
        let linked = api::emission_day(storage.clone(), emission_day)
            .unwrap()
            .unwrap();
        assert_eq!(linked.wwd, wwd);
        assert_eq!(linked.state, metadosis_state::PENDING);

        let snapshot_ts = proj.forming_end + 12 * 3_600;
        advance_at(&storage, snapshot_ts, genesis_ts);
        let factory = TributeFactoryContract::new(storage.clone());
        assert!(factory.forming_snapshot_finalized.read(&wwd).unwrap());
        assert!(!outbe_oracle::api::has_worldwide_day_vwap_snapshot(storage.clone(), wwd).unwrap());
        assert_eq!(
            api::emission_day(storage.clone(), emission_day)
                .unwrap()
                .unwrap()
                .state,
            metadosis_state::PENDING
        );

        storage
            .set_block_timestamp(U256::from(proj.offerings_start))
            .unwrap();
        assert!(!api::is_offering_day(storage.clone(), wwd).unwrap());
        let offer_err =
            outbe_tributefactory::worldwideday::genesis::submit_signed_test_offer_for_day(
                storage.clone(),
                wwd,
            )
            .expect_err("offer_tribute must revert when forming VWAP snapshot is absent");
        assert_eq!(
            offer_err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_tributefactory::errors::TributeFactoryError::WorldwideDayNotOffering {
                    worldwide_day: wwd,
                }
            )
            .to_string()
        );

        let expected_limit = outbe_emission::day_emission::emission_limit_of_day(4);
        let settle_ts = date_key_to_utc_timestamp(20261006).unwrap() + 1;
        storage.set_block_timestamp(U256::from(settle_ts)).unwrap();
        let ctx_06 = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: settle_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: genesis_ts,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        allocate_to_metadosis(&ctx_06, emission_day, expected_limit);
        outbe_tributefactory::worldwideday::advance_worldwide_days_command(&ctx_06).unwrap();
        assert!(TributeContract::new(storage.clone())
            .is_day_sealed(wwd)
            .unwrap());

        crate::commands::process_emission_day(&ctx_06, emission_day).unwrap();
        let settled = api::emission_day(storage.clone(), emission_day)
            .unwrap()
            .unwrap();
        assert_eq!(settled.state, metadosis_state::COMPLETED);
        assert_eq!(settled.wwd, wwd);
        assert_eq!(
            TributeContract::new(storage.clone())
                .get_day_totals(wwd)
                .unwrap()
                .tribute_count,
            0
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage)
                .promis_limit()
                .unwrap(),
            expected_limit
        );
    });

    let limit_set = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisLimitSet::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(limit_set.len(), 1);
    assert_eq!(limit_set[0].emissionDay, 20261005);

    let completed = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisCompleted::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].emissionDay, 20261005);
    assert_eq!(completed[0].status, "COMPLETED");

    let canceled_or_failed = provider
        .get_ordered_events()
        .iter()
        .filter(|event| {
            IMetadosis::MetadosisCanceled::decode_log(event).is_ok()
                || IMetadosis::MetadosisFailed::decode_log(event).is_ok()
        })
        .count();
    assert_eq!(canceled_or_failed, 0);
}

#[test]
fn zero_metadosis_limit_cancels_day_with_reason_one_and_preserves_cancelled_state() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 64);
    install_test_enclaves();
    StorageHandle::enter(&mut provider, |storage| {
        let emission_day = 20261001u32;
        outbe_emission::api::record_day_allocation(
            storage.clone(),
            emission_day,
            outbe_emission::api::DayAllocationAmounts {
                emission_limit: U256::from(100u64),
                metadosis_limit: U256::ZERO,
                validators: U256::from(25u64),
                waa: U256::from(25u64),
                sra: U256::from(25u64),
                cca: U256::from(25u64),
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();
        let due_ts = date_key_to_utc_timestamp(20261002).unwrap() + 1;
        storage.set_block_timestamp(U256::from(due_ts)).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: due_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(due_ts, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        crate::commands::process_emission_day(&ctx, emission_day).unwrap();

        let record = outbe_emission::api::get_emission_day_record(storage, emission_day)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.metadosis_state,
            outbe_emission::schema::metadosis_state::CANCELLED
        );
        assert_eq!(
            record.state,
            outbe_emission::schema::emission_day_state::COMPLETED
        );
    });

    let canceled = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisCanceled::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(canceled.len(), 1);
    assert_eq!(canceled[0].emissionDay, 20261001);
    assert_eq!(canceled[0].reason, 1);
}
