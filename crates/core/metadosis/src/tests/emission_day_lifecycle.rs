use super::*;
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
    advance(&ctx);
}

/// The WWD step of a Cycle run: it opens the WWDs and the Cycle makes their records.
fn advance(ctx: &BlockRuntimeContext<'_>) {
    for wwd in outbe_worldwideday::advance_worldwide_days_command(ctx).unwrap() {
        let (schedule, _) = outbe_worldwideday::compute_wwd_schedule(wwd).unwrap();
        if outbe_emission::api::get_emission_day(ctx.storage.clone(), schedule.emission_day)
            .unwrap()
            .is_none()
        {
            outbe_emission::api::create_emission_day(ctx, schedule.emission_day, wwd).unwrap();
        }
    }
}

/// The Emission Limit and the Metadosis limit of the record of `day`, `day_number` days after
/// the genesis day.
fn limits_of(day_number: u32) -> (u128, u128) {
    let limit = outbe_emission::day_emission::emission_limit_of_day(day_number);
    let shares = outbe_emission::api::compute_emission_shares(limit).unwrap();
    (limit.to(), shares.metadosis.to())
}

#[test]
fn init_genesis_day_creates_the_pipeline_lookahead_with_bidirectional_links_and_closed_form_limits()
{
    with_storage(|storage| {
        let genesis_ts = date_key_to_utc_timestamp(20260920).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);

        let opened = WorldwideDayContract::new(storage.clone())
            .pipeline_wwd
            .read_all()
            .unwrap();
        assert_eq!(opened, vec![WorldwideDay::new(20260920)]);

        for day in [20260921, 20260922, 20260923, 20260924, 20260925] {
            advance_at(
                &storage,
                date_key_to_utc_timestamp(day).unwrap(),
                genesis_ts,
            );
        }

        let pipeline = WorldwideDayContract::new(storage.clone())
            .pipeline_wwd
            .read_all()
            .unwrap();
        assert_eq!(pipeline.len(), 3);
        assert!(!pipeline.contains(&WorldwideDay::new(20260922)));
        assert!(pipeline.contains(&WorldwideDay::new(20260925)));

        // Verify wwd = 20260920 -> day = 20260921 (and wwd = 20260925 -> day = 20260926)
        let wwd_20 = outbe_worldwideday::api::get_worldwide_day(
            storage.clone(),
            WorldwideDay::new(20260920),
        )
        .unwrap()
        .unwrap();
        assert_eq!(wwd_20.emission_day, 20260921);

        let ed_21 = outbe_emission::api::get_emission_day(storage.clone(), 20260921)
            .unwrap()
            .unwrap();
        assert_eq!(ed_21.day, 20260921);
        assert_eq!(ed_21.wwd, WorldwideDay::new(20260920));
        assert_eq!(
            ed_21.metadosis_start,
            date_key_to_utc_timestamp(next_date_key(20260921).expect("date step")).unwrap()
                + 12 * 3_600
        );
        assert_eq!(ed_21.metadosis_state, metadosis_state::PENDING);
        assert_eq!((ed_21.emission_limit, ed_21.metadosis_limit), limits_of(1));

        let wwd_25 = outbe_worldwideday::api::get_worldwide_day(
            storage.clone(),
            WorldwideDay::new(20260925),
        )
        .unwrap()
        .unwrap();
        assert_eq!(wwd_25.emission_day, 20260926);
        let ed_26 = outbe_emission::api::get_emission_day(storage.clone(), 20260926)
            .unwrap()
            .unwrap();
        assert_eq!(ed_26.wwd, WorldwideDay::new(20260925));
        assert_eq!((ed_26.emission_limit, ed_26.metadosis_limit), limits_of(6));

        // 20260920..=20260925 stay linked after the daily steps. A WWD is sealed when its
        // offering ends and leaves the pipeline; the record and its emission day remain.
        let mut wwd_key = 20260920;
        for _ in 0..6u32 {
            let wwd = WorldwideDay::new(wwd_key);
            let wwd_record = outbe_worldwideday::api::get_worldwide_day(storage.clone(), wwd)
                .unwrap()
                .unwrap();
            let ed =
                outbe_emission::api::get_emission_day(storage.clone(), wwd_record.emission_day)
                    .unwrap()
                    .unwrap();
            assert_eq!(ed.wwd, wwd);
            assert_eq!(
                ed.metadosis_start,
                date_key_to_utc_timestamp(next_date_key(ed.day).expect("date step")).unwrap()
                    + 12 * 3_600
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

        let wwd_record = outbe_worldwideday::api::get_worldwide_day(storage.clone(), wwd_sep_01)
            .unwrap()
            .unwrap();
        assert_eq!(
            WorldwideDay::from_timestamp(wwd_record.offering_start),
            WorldwideDay::new(20260902)
        );
        assert_eq!(
            wwd_record.offering_end,
            date_key_to_utc_timestamp(20260903).unwrap() + 12 * 3_600
        );
        assert_eq!(wwd_record.emission_day, 20260902);

        let expected_metadosis_start = date_key_to_utc_timestamp(20260903).unwrap() + 12 * 3_600;
        assert_eq!(expected_metadosis_start, wwd_record.offering_end);

        let ed_02 = outbe_emission::api::get_emission_day(storage.clone(), 20260902)
            .unwrap()
            .unwrap();
        assert_eq!(ed_02.day, 20260902);
        assert_eq!(ed_02.wwd, wwd_sep_01);
        assert_eq!(ed_02.metadosis_start, expected_metadosis_start);
        assert_eq!((ed_02.emission_limit, ed_02.metadosis_limit), limits_of(1));

        // Nested process-ready checks recompute the lease from the provider clock
        // and block. Allocate day 20260902 into Processing.
        allocate_to_metadosis(&ctx, 20260902, U256::from(1_000_000u64));

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
        advance(&at_start);

        // Simulate a halt from 3 Sep to 8 Sep 2026: the first block after restart
        // still runs the Metadosis of 20260902.
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
        let briefed = outbe_system_common::time::EmissionDate::new(20260902);
        outbe_desis::test_support::take_day(storage.clone(), briefed).unwrap();
        crate::commands::start_metadosis(&aligned, 20260902).unwrap();
        let completed = outbe_emission::api::get_emission_day(storage.clone(), 20260902)
            .unwrap()
            .unwrap();
        assert_eq!(completed.day, 20260902);
        assert_eq!(completed.wwd, wwd_sep_01);
        assert_eq!(completed.metadosis_state, metadosis_state::COMPLETED);
        let desis = outbe_desis::schema::DesisContract::new(storage);
        assert_eq!(desis.desis_limit_set.read(&briefed).unwrap(), 1);
        assert_eq!(desis.has_desis_limit.read(&briefed).unwrap(), 0);
    });
}

fn genesis_ctx<'a>(storage: &StorageHandle<'a>, timestamp: u64) -> BlockRuntimeContext<'a> {
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
    BlockRuntimeContext::new(
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: storage.block_number().unwrap(),
            timestamp,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: u64::max(timestamp, 1),
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
        storage.clone(),
    )
}

#[test]
fn first_wwd_for_genesis_splits_0_or_1_empty_emission_day_across_noon_utc() {
    // Genesis on 2026-10-01 at 11:59 UTC -> first_wwd = 20260930 (G - 1), whose
    // emission day is G: no empty EmissionDay.
    with_storage(|storage| {
        let genesis_before_noon =
            date_key_to_utc_timestamp(20261001).unwrap() + 11 * 3_600 + 59 * 60;
        assert_eq!(
            outbe_worldwideday::first_wwd_for_genesis(genesis_before_noon),
            WorldwideDay::new(20260930)
        );
        let ctx = genesis_ctx(&storage, genesis_before_noon);
        advance(&ctx);

        let first_wwd = outbe_worldwideday::api::get_worldwide_day(
            storage.clone(),
            WorldwideDay::new(20260930),
        )
        .unwrap()
        .unwrap();
        assert_eq!(first_wwd.emission_day, 20261001);
    });

    // Genesis on 2026-10-01 at 12:01 UTC -> first_wwd = 20261001 (G), whose
    // emission day is G + 1: one empty EmissionDay (20261001).
    with_storage(|storage| {
        let genesis_after_noon = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600 + 60;
        assert_eq!(
            outbe_worldwideday::first_wwd_for_genesis(genesis_after_noon),
            WorldwideDay::new(20261001)
        );
        let ctx = genesis_ctx(&storage, genesis_after_noon);
        advance(&ctx);

        let first_wwd = outbe_worldwideday::api::get_worldwide_day(
            storage.clone(),
            WorldwideDay::new(20261001),
        )
        .unwrap()
        .unwrap();
        assert_eq!(first_wwd.emission_day, 20261002);
        // The genesis day has no WWD: the first block makes its record with none.
        let shell = outbe_emission::api::create_emission_day(&ctx, 20261001, WorldwideDay::new(0))
            .expect("empty day record");
        assert_eq!(shell.wwd, WorldwideDay::new(0));

        allocate_to_metadosis(&ctx, 20261001, U256::from(1_000_000u64));
        let empty_due_ctx = genesis_ctx(
            &storage,
            date_key_to_utc_timestamp(20261002).unwrap() + 12 * 3_600,
        );
        crate::commands::start_metadosis(&empty_due_ctx, 20261001).unwrap();
        let settled_empty = outbe_emission::api::get_emission_day(storage.clone(), 20261001)
            .unwrap()
            .unwrap();
        assert_eq!(settled_empty.wwd, WorldwideDay::new(0));
        assert_eq!(settled_empty.metadosis_state, metadosis_state::COMPLETED);

        // Catch-up at 2026-10-11 12:00 UTC opens every WWD up to that date whose offering has
        // not ended: 20261010..=20261011.
        let catchup_ts = date_key_to_utc_timestamp(20261011).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(catchup_ts)).unwrap();
        let catchup_ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: storage.block_number().unwrap(),
                timestamp: catchup_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: genesis_after_noon,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        advance(&catchup_ctx);
        for wwd_key in 20261010..=20261011 {
            let wwd_record = outbe_worldwideday::api::get_worldwide_day(
                storage.clone(),
                WorldwideDay::new(wwd_key),
            )
            .unwrap()
            .expect("catch-up and lookahead WWD must exist");
            let ed =
                outbe_emission::api::get_emission_day(storage.clone(), wwd_record.emission_day)
                    .unwrap()
                    .expect("linked emission day must exist");
            assert_eq!(ed.wwd, WorldwideDay::new(wwd_key));
        }
    });
}

#[test]
fn wwd_takes_offers_only_by_the_time_and_seals_on_close() {
    with_storage(|storage| {
        let wwd = WorldwideDay::new(20261001);
        let genesis_ts = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);

        let factory = WorldwideDayContract::new(storage.clone());
        let wwd_record = outbe_worldwideday::api::get_worldwide_day(storage.clone(), wwd)
            .unwrap()
            .unwrap();
        assert_eq!(
            wwd_record.forming_end().unwrap(),
            date_key_to_utc_timestamp(20261002).unwrap() + 12 * 3_600
        );
        assert_eq!(
            wwd_record.offering_start,
            date_key_to_utc_timestamp(20261001).unwrap() + 10 * 3_600
        );
        assert_eq!(
            wwd_record.offering_end,
            date_key_to_utc_timestamp(20261003).unwrap() + 12 * 3_600
        );
        assert!(!outbe_worldwideday::is_worldwide_day_sealed(storage.clone(), wwd).unwrap());

        // The day takes offers by the time, with no oracle price.
        storage
            .set_block_timestamp(U256::from(wwd_record.offering_start))
            .unwrap();
        assert!(outbe_worldwideday::api::is_offering_day(storage.clone(), wwd).unwrap());
        assert!(
            outbe_worldwideday::api::offering_worldwide_days(storage.clone())
                .unwrap()
                .contains(&wwd)
        );
        storage
            .set_block_timestamp(U256::from(wwd_record.offering_end))
            .unwrap();
        assert!(!outbe_worldwideday::api::is_offering_day(storage.clone(), wwd).unwrap());

        // The step at the offering end, 2026-10-03 12:00 UTC, seals the WWD.
        let seal_ts = wwd_record.offering_end;
        advance_at(&storage, seal_ts, genesis_ts);
        assert!(!factory.pipeline_wwd.read_all().unwrap().contains(&wwd));
        assert!(outbe_worldwideday::is_worldwide_day_sealed(storage.clone(), wwd).unwrap());
    });
}

#[test]
fn wwd_without_price_runs_as_empty_day() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 64);
    install_test_enclaves();
    StorageHandle::enter(&mut provider, |storage| {
        let wwd = WorldwideDay::new(20261001);
        let emission_day = 20261002u32;
        let genesis_ts = date_key_to_utc_timestamp(20261001).unwrap() + 12 * 3_600;
        advance_at(&storage, genesis_ts, genesis_ts);

        let wwd_record = outbe_worldwideday::api::get_worldwide_day(storage.clone(), wwd)
            .unwrap()
            .unwrap();
        assert_eq!(wwd_record.emission_day, emission_day);
        let linked = outbe_emission::api::get_emission_day(storage.clone(), emission_day)
            .unwrap()
            .unwrap();
        assert_eq!(linked.wwd, wwd);
        assert_eq!(linked.metadosis_state, metadosis_state::PENDING);

        let step_ts = wwd_record.forming_end().unwrap() + 12 * 3_600;
        advance_at(&storage, step_ts, genesis_ts);
        assert_eq!(
            outbe_emission::api::get_emission_day(storage.clone(), emission_day)
                .unwrap()
                .unwrap()
                .metadosis_state,
            metadosis_state::PENDING
        );

        storage
            .set_block_timestamp(U256::from(wwd_record.offering_start))
            .unwrap();
        assert!(outbe_worldwideday::api::is_offering_day(storage.clone(), wwd).unwrap());

        let expected_limit = outbe_emission::day_emission::emission_limit_of_day(1);
        let settle_ts = date_key_to_utc_timestamp(20261003).unwrap() + 12 * 3_600;
        storage.set_block_timestamp(U256::from(settle_ts)).unwrap();
        let ctx_03 = BlockRuntimeContext::new(
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
        allocate_to_metadosis(&ctx_03, emission_day, expected_limit);
        advance(&ctx_03);
        assert!(outbe_worldwideday::is_worldwide_day_sealed(storage.clone(), wwd).unwrap());

        crate::commands::start_metadosis(&ctx_03, emission_day).unwrap();
        advance_at(
            &storage,
            date_key_to_utc_timestamp(20261004).unwrap(),
            genesis_ts,
        );
        assert!(outbe_worldwideday::is_worldwide_day_sealed(storage.clone(), wwd).unwrap());
        let settled = outbe_emission::api::get_emission_day(storage.clone(), emission_day)
            .unwrap()
            .unwrap();
        assert_eq!(settled.metadosis_state, metadosis_state::COMPLETED);
        assert_eq!(settled.wwd, wwd);
        assert_eq!(
            outbe_worldwideday::worldwide_day_totals(storage.clone(), wwd)
                .unwrap()
                .nod_count,
            0
        );
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage)
                .promis_limit()
                .unwrap(),
            expected_limit
        );
    });

    let completed = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisCompleted::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].emissionDay, 20261002);

    let failed = provider
        .get_ordered_events()
        .iter()
        .filter(|event| IMetadosis::MetadosisFailed::decode_log(event).is_ok())
        .count();
    assert_eq!(failed, 0);
}
