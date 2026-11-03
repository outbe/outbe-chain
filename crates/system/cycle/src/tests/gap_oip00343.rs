//! OIP-00343 proof tests for `ED01`, `ED03`, and `ED54`.

use alloy_primitives::U256;
use alloy_sol_types::{SolError, SolEvent};
use outbe_emission::day_emission::day_emission_limit;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    address_pair::AddressPair,
    addresses::TRIBUTE_FACTORY_ADDRESS,
    error::PrecompileError,
    storage::MetadosisMutationPurposeTag,
    time::{
        date_key_to_utc_timestamp, day_number_between, timestamp_to_date_key, WorldwideDay,
        SECONDS_PER_DAY,
    },
};
use outbe_tributefactory::{precompile::ITributeFactory, TributeFactoryContract};

use super::*;

#[test]
fn gap_ed01_every_utc_day_is_an_emission_day() {
    // Genesis at 2026-10-01 00:01:00 UTC (date key 20261001).
    // The first WWD is 20260930 (forming_end = 2026-10-01 12:00 UTC > genesis), whose E = D+4 is 20261004.
    // Days 20261001, 20261002, 20261003 have wwd == 0; day 20261004 has wwd == 20260930 (with 0 Tributes).
    let genesis_oct_1 = date_key_to_utc_timestamp(20_261_001).unwrap() + 60;
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);

    storage.enter(|handle| {
        let block = BlockContext::new(1, genesis_oct_1, CHAIN_ID, Address::ZERO, Vec::new())
            .with_genesis_timestamp(genesis_oct_1);
        let ctx = runtime_context(block, handle);
        run_cycle_lifecycle(&ctx).unwrap();
    });

    // Advance hour by hour across 4 full UTC days (20261001..=20261004) into 20261005 02:00 UTC
    // with zero Tributes and zero oracle prices.
    let first_hour = (genesis_oct_1 / 3_600 + 1) * 3_600;
    let end_ts = date_key_to_utc_timestamp(20_261_005).unwrap() + 2 * 3_600;
    let mut block_number = 2u64;
    let mut ts = first_hour;
    while ts <= end_ts {
        storage.set_block_number(block_number);
        storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
        storage.enter(|handle| {
            let block = BlockContext::new(block_number, ts, CHAIN_ID, Address::ZERO, Vec::new())
                .with_genesis_timestamp(genesis_oct_1);
            let ctx = runtime_context(block, handle);
            account_parent(&ctx, block_number);
            dispatch_triggers(&ctx).unwrap();
            crate::handler::credit_reward_slice(&ctx).unwrap();
        });
        block_number += 1;
        ts += 3_600;
    }

    storage.enter(|handle| {
        let expected_days = [
            (20_261_001u32, 0u32, WorldwideDay::new(0)),
            (20_261_002u32, 1u32, WorldwideDay::new(0)),
            (20_261_003u32, 2u32, WorldwideDay::new(0)),
            (20_261_004u32, 3u32, WorldwideDay::new(20_260_930)),
        ];
        let mut expected_promis_total = U256::ZERO;
        for (day, day_index, expected_wwd) in expected_days {
            let record = outbe_emission::api::get_emission_day_record(handle.clone(), day)
                .unwrap()
                .unwrap_or_else(|| panic!("missing EmissionDay record for {day}"));
            assert_eq!(record.wwd, expected_wwd, "unexpected wwd for day {day}");
            let limit = day_emission_limit(day_index);
            assert_eq!(
                U256::from(record.emission_limit),
                limit,
                "day {day} must receive day_emission_limit({day_index})"
            );
            assert_eq!(
                record.state,
                outbe_emission::schema::emission_day_state::COMPLETED,
                "empty day {day} must complete"
            );
            assert_eq!(
                record.metadosis_state,
                outbe_emission::schema::metadosis_state::COMPLETED,
                "empty day {day} metadosis must complete"
            );
            let distributed_and_promis = U256::from(record.validators)
                + U256::from(record.waa)
                + U256::from(record.sra)
                + U256::from(record.cca)
                + U256::from(record.nods)
                + U256::from(record.promis_limit);
            assert_eq!(
                distributed_and_promis, limit,
                "distributed pools + PromisLimit must conserve day_emission_limit({day_index}) on day {day}"
            );
            expected_promis_total += U256::from(record.promis_limit);
        }
        let actual_promis_total = outbe_promislimit::PromisLimitContract::new(handle)
            .promis_limit()
            .unwrap();
        assert_eq!(
            actual_promis_total, expected_promis_total,
            "PromisLimit contract balance must equal the sum of per-day promis_limit transfers"
        );
    });
}

#[test]
fn gap_ed03_start_date_comes_from_genesis_block() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 32);

    // 1. Over the exact same storage (no stored start_date slot), two BlockContexts with
    //    different genesis_timestamp values derive different start dates, active UTC days,
    //    and day_number values.
    storage.enter(|handle| {
        let g1_midnight = date_key_to_utc_timestamp(20_240_101).unwrap();
        let g1_end_of_day = g1_midnight + SECONDS_PER_DAY - 1; // 23:59:59 UTC
        let g2 = date_key_to_utc_timestamp(20_240_105).unwrap() + SECONDS_PER_DAY - 1;
        let block_ts = date_key_to_utc_timestamp(20_240_110).unwrap() + 3_600;
        let block_day = timestamp_to_date_key(block_ts);

        // 00:00:00 UTC and 23:59:59 UTC of the same day produce the exact same start_date,
        // while 00:00:00 UTC of the next day advances start_date by +1.
        assert_eq!(timestamp_to_date_key(g1_midnight), 20_240_101);
        assert_eq!(timestamp_to_date_key(g1_end_of_day), 20_240_101);
        assert_eq!(timestamp_to_date_key(g1_end_of_day + 1), 20_240_102);

        let active_from_g1 =
            crate::handler::derive_active_utc_day(handle.clone(), g1_midnight, block_ts).unwrap();
        let active_from_g2 =
            crate::handler::derive_active_utc_day(handle.clone(), g2, block_ts).unwrap();
        assert_eq!(active_from_g1, 20_240_101);
        assert_eq!(active_from_g2, 20_240_105);
        assert_eq!(day_number_between(active_from_g1, block_day).unwrap(), 9);
        assert_eq!(day_number_between(active_from_g2, block_day).unwrap(), 5);

        // Across 30 consecutive days, start_date derived from genesis_timestamp is invariant
        // and day_number_between increments by 1 each UTC day.
        let start_date = timestamp_to_date_key(g1_end_of_day);
        for offset in 0..=30u32 {
            let ts_k = g1_midnight + u64::from(offset) * SECONDS_PER_DAY + 12 * 3_600;
            assert_eq!(timestamp_to_date_key(g1_end_of_day), start_date);
            assert_eq!(
                day_number_between(start_date, timestamp_to_date_key(ts_k)).unwrap(),
                offset
            );
        }

        // 2. genesis_timestamp == 0 is rejected as invalid.
        let uninitialized =
            crate::handler::derive_active_utc_day(handle.clone(), 0, block_ts).unwrap();
        assert_eq!(uninitialized, 0);
        let ctx_zero_genesis = runtime_context(
            BlockContext::new(2, block_ts, CHAIN_ID, Address::ZERO, Vec::new())
                .with_genesis_timestamp(0),
            handle.clone(),
        );
        let err = crate::handler::run_protocol_cycle(&ctx_zero_genesis)
            .expect_err("zero genesis_timestamp must fail");
        assert!(matches!(err, PrecompileError::Fatal(_)));
    });

    // 3. Cycle runs from block 1 and settles day 0 at the next UTC midnight without any
    //    pre-seeded Rewards start_date slot.
    storage.enter(|handle| {
        let block_1 = runtime_context(block_ctx(1, GENESIS_TS + 60), handle);
        run_cycle_lifecycle(&block_1).unwrap();
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 3_600;
        let ctx = dispatch_at(handle.clone(), 2, fire_ts);
        crate::handler::credit_reward_slice(&ctx).unwrap();
        let record = outbe_emission::api::get_emission_day_record(handle, 20_240_101)
            .unwrap()
            .expect("genesis day must be allocated");
        assert_eq!(U256::from(record.emission_limit), day_emission_limit(0));
    });
}

#[test]
fn gap_ed54_wwd_step_runs_once_a_day() {
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 64);

    // Block 1 at 2024-01-01 00:01:00 UTC initializes WWDs for cutoff 2024-01-01 00:00:00 UTC:
    // pipeline contains 20231231 (first_wwd_for_genesis), 20240101, and 20240102,
    // and last_wwd_advance_day == 20240101.
    storage.enter(|handle| {
        let mut oracle = OracleContract::new(handle.clone());
        // Seed an oracle price inside WWD 20240101's forming window [2023-12-31 10:00 UTC, 2024-01-02 12:00 UTC)
        // so step A6 at 2024-01-03 00:00 UTC snapshots it directly from Oracle.
        let wwd_20240101_start = WorldwideDay::new(20_240_101).start_timestamp().unwrap();
        oracle
            .write_snapshot(
                wwd_20240101_start + 3_600,
                &[(AddressPair::new_coen_to(840), U256::from(100), U256::ONE)],
            )
            .unwrap();

        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_lifecycle(&anchor).unwrap();
        let factory = TributeFactoryContract::new(handle);
        assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_101);
        assert_eq!(
            factory.pipeline_wwd.read_all().unwrap(),
            vec![
                WorldwideDay::new(20_231_231),
                WorldwideDay::new(20_240_101),
                WorldwideDay::new(20_240_102),
            ]
        );
    });

    // Intraday hourly tick on 2024-01-01 at 13:00 UTC is a no-op for WWD advancement.
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ts_13h = GENESIS_TS + 13 * 3_600;
        let ctx = dispatch_at(handle.clone(), 2, ts_13h);
        let factory = TributeFactoryContract::new(ctx.storage.clone());
        assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_101);
        assert_eq!(
            factory.pipeline_wwd.read_all().unwrap(),
            vec![
                WorldwideDay::new(20_231_231),
                WorldwideDay::new(20_240_101),
                WorldwideDay::new(20_240_102),
            ]
        );
    });

    // Advance day by day to 2024-01-05:
    // - On 2024-01-02 00:00 UTC (day_offset = 1), WWD 20240101 forming_end (2024-01-02 12:00 UTC) > cutoff_ts,
    //   so forming_snapshot_finalized[20240101] is still false.
    // - On 2024-01-03 00:00 UTC (day_offset = 2), A6 snapshots WWD 20240101's forming VWAP from Oracle.
    // - On 2024-01-05 00:00 UTC (day_offset = 4), WWD 20231231 closes/seals, leaving 20240101..=20240106.
    let mut block_no = 3u64;
    for day_offset in 1..=4u64 {
        let midnight_ts = GENESIS_TS + day_offset * SECONDS_PER_DAY;
        storage.set_block_number(block_no);
        storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
        storage.enter(|handle| {
            let ctx = dispatch_at(handle.clone(), block_no, midnight_ts);
            crate::handler::credit_reward_slice(&ctx).unwrap();
            let expected_day = timestamp_to_date_key(midnight_ts);
            let factory = TributeFactoryContract::new(ctx.storage.clone());
            assert_eq!(factory.last_wwd_advance_day.read().unwrap(), expected_day);
            if day_offset == 1 {
                assert!(!factory
                    .forming_snapshot_finalized
                    .read(&WorldwideDay::new(20_240_101))
                    .unwrap());
            }
            if day_offset == 2 {
                assert!(
                    factory
                        .forming_snapshot_finalized
                        .read(&WorldwideDay::new(20_240_101))
                        .unwrap(),
                    "A6 at 2024-01-03 00:00 UTC must finalize forming snapshot of WWD 20240101"
                );
                assert!(
                    outbe_oracle::api::has_worldwide_day_vwap_snapshot(
                        ctx.storage.clone(),
                        WorldwideDay::new(20_240_101)
                    )
                    .unwrap(),
                    "A6 at 2024-01-03 00:00 UTC must store forming VWAP snapshot of WWD 20240101"
                );
            }
        });
        block_no += 1;
    }

    // Verify exact offering window boundaries [2024-01-03 10:00 UTC, 2024-01-05 12:00 UTC) for WWD 20240101:
    // - 2024-01-03 09:59:59 UTC -> false
    // - 2024-01-03 10:00:00 UTC -> true
    // - 2024-01-05 11:59:59 UTC -> true
    // - 2024-01-05 12:00:00 UTC -> false
    storage.enter(|handle| {
        let offering_start = GENESIS_TS + 2 * SECONDS_PER_DAY + 10 * 3_600;
        let offering_end = GENESIS_TS + 4 * SECONDS_PER_DAY + 12 * 3_600;
        for (ts, expected) in [
            (offering_start - 1, false),
            (offering_start, true),
            (offering_end - 1, true),
            (offering_end, false),
        ] {
            handle.set_block_timestamp(U256::from(ts)).unwrap();
            assert_eq!(
                outbe_tributefactory::worldwideday::is_offering_day(
                    handle.clone(),
                    WorldwideDay::new(20_240_101)
                )
                .unwrap(),
                expected,
                "unexpected is_offering_day at ts={ts}"
            );
        }
    });

    // Run all intraday hourly ticks 01:00..=23:00 UTC on 2024-01-05:
    // - No new WorldwideDayCreated or WorldwideDayClosed events are emitted at any intraday hour.
    // - At 13:00 UTC (after WWD 20240101 offering_end = 12:00 UTC and after WWD 20240104 forming_end = 12:00 UTC):
    //   * `is_offering_day(20240101)` is false by block timestamp (`now >= offering_end`).
    //   * `is_day_sealed(20240101)` is still false and 20240101 remains in `pipeline_wwd`.
    //   * `forming_snapshot_finalized[20240104]` is still false (forming snapshot is taken at 00:00 UTC).
    let events_before_intraday = storage.get_events(TRIBUTE_FACTORY_ADDRESS).len();
    for hour in 1..=23u64 {
        let ts_hour = GENESIS_TS + 4 * SECONDS_PER_DAY + hour * 3_600;
        storage.set_block_number(block_no);
        storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
        storage.enter(|handle| {
            let ctx = dispatch_at(handle.clone(), block_no, ts_hour);
            crate::handler::credit_reward_slice(&ctx).unwrap();
            let factory = TributeFactoryContract::new(ctx.storage.clone());
            assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_105);
            if hour == 13 {
                assert!(!outbe_tributefactory::worldwideday::is_offering_day(
                    ctx.storage.clone(),
                    WorldwideDay::new(20_240_101)
                )
                .unwrap());
                assert!(
                    !outbe_tribute::TributeContract::new(ctx.storage.clone())
                        .is_day_sealed(WorldwideDay::new(20_240_101))
                        .unwrap(),
                    "WWD 20240101 must not be sealed at 13:00 UTC; A6 seals at 00:00 UTC"
                );
                assert!(
                    factory
                        .pipeline_wwd
                        .read_all()
                        .unwrap()
                        .contains(&WorldwideDay::new(20_240_101)),
                    "WWD 20240101 must remain in pipeline at 13:00 UTC; A6 runs at 00:00 UTC"
                );
                assert!(
                    !factory
                        .forming_snapshot_finalized
                        .read(&WorldwideDay::new(20_240_104))
                        .unwrap(),
                    "WWD 20240104 forming snapshot must not be finalized at 13:00 UTC"
                );
            }
        });
        block_no += 1;
    }
    assert_eq!(
        storage.get_events(TRIBUTE_FACTORY_ADDRESS).len(),
        events_before_intraday,
        "intraday ticks 01:00..=23:00 UTC must not emit duplicate TributeFactory WWD events"
    );

    // Force an A6 revert at 2024-01-06 00:00 UTC by pre-linking future EmissionDay 20240111
    // (the E = D+4 target of WWD 20240107) to a conflicting WWD, so creating WWD 20240107 inside
    // `advance_worldwide_days` reverts with `RevertBytes`.
    // Verify:
    // - Step A4 (`settle_emission_day(20240105)`) still commits (`state == PROCESSING`).
    // - Step A6 rolls back its inner checkpoint (`last_wwd_advance_day` stays 20240105, WWD 20240101
    //   remains unsealed in `pipeline_wwd`) and emits `WorldwideDayAdvanceFailed` with non-empty reason.
    // - Step A7 (`start_metadosis`) runs cleanly and defers EmissionDay 20240105 because WWD 20240101
    //   is not yet sealed (`metadosis_state == PENDING`).
    storage.enter(|handle| {
        outbe_emission::api::link_wwd(handle, 20_240_111, WorldwideDay::new(20_240_120)).unwrap();
    });
    let ts_day6_00h = GENESIS_TS + 5 * SECONDS_PER_DAY;
    storage.set_block_number(block_no);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), block_no, ts_day6_00h);
        let factory = TributeFactoryContract::new(ctx.storage.clone());
        assert_eq!(
            factory.last_wwd_advance_day.read().unwrap(),
            20_240_105,
            "reverted A6 must roll back and leave last_wwd_advance_day at 20240105"
        );
        assert!(
            factory
                .pipeline_wwd
                .read_all()
                .unwrap()
                .contains(&WorldwideDay::new(20_240_101)),
            "reverted A6 must roll back closing/sealing of WWD 20240101"
        );
        let ed_20240105 =
            outbe_emission::api::get_emission_day_record(ctx.storage.clone(), 20_240_105)
                .unwrap()
                .expect("A4 must still allocate 20240105 before A6");
        assert_eq!(
            ed_20240105.state,
            outbe_emission::schema::emission_day_state::PROCESSING,
            "A4 allocation of 20240105 must remain committed when A6 reverts"
        );
        assert_eq!(
            ed_20240105.metadosis_state,
            outbe_emission::schema::metadosis_state::PENDING,
            "A7 must defer 20240105 while WWD 20240101 is unsealed"
        );
    });
    let failed_event = storage
        .get_events(TRIBUTE_FACTORY_ADDRESS)
        .iter()
        .find_map(|log| ITributeFactory::WorldwideDayAdvanceFailed::decode_log_data(log).ok())
        .expect("WorldwideDayAdvanceFailed must be emitted when A6 reverts");
    assert_eq!(failed_event.utcDay, 20_240_106);
    assert_eq!(failed_event.blockTimestamp, ts_day6_00h);
    assert!(
        !failed_event.reason.is_empty(),
        "WorldwideDayAdvanceFailed.reason must carry the revert payload"
    );
    block_no += 1;

    // Clear the conflicting pre-link on 20240111 and advance to the next hourly tick (2024-01-06 01:00 UTC).
    // Now A6 retries, seals/closes WWD 20240101, finalizes forming snapshot of WWD 20240104, creates WWD 20240107,
    // updates `last_wwd_advance_day = 20240106`, and A7 immediately settles EmissionDay 20240105 in the same cycle!
    storage.enter(|handle| {
        let mut clear = outbe_emission::api::get_emission_day_record(handle.clone(), 20_240_111)
            .unwrap()
            .unwrap();
        clear.wwd = WorldwideDay::new(0);
        outbe_emission::schema::EmissionContract::new(handle)
            .emission_days
            .update(&clear)
            .unwrap();
    });
    let ts_day6_01h = ts_day6_00h + 3_600;
    storage.set_block_number(block_no);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), block_no, ts_day6_01h);
        crate::handler::credit_reward_slice(&ctx).unwrap();
        let factory = TributeFactoryContract::new(ctx.storage.clone());
        assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_106);
        assert!(
            !factory
                .pipeline_wwd
                .read_all()
                .unwrap()
                .contains(&WorldwideDay::new(20_240_101)),
            "A6 retry at 01:00 UTC must close/seal WWD 20240101"
        );
        assert!(
            outbe_tribute::TributeContract::new(ctx.storage.clone())
                .is_day_sealed(WorldwideDay::new(20_240_101))
                .unwrap(),
            "A6 retry at 01:00 UTC must seal WWD 20240101 in Tribute"
        );
        assert!(
            factory
                .forming_snapshot_finalized
                .read(&WorldwideDay::new(20_240_104))
                .unwrap(),
            "A6 retry at 01:00 UTC must finalize forming snapshot of WWD 20240104"
        );
        assert!(
            factory
                .worldwide_days
                .get(WorldwideDay::new(20_240_107))
                .unwrap()
                .is_some(),
            "A6 retry at 01:00 UTC must create WWD X+1 (20240107)"
        );
        let ed_20240105 =
            outbe_emission::api::get_emission_day_record(ctx.storage.clone(), 20_240_105)
                .unwrap()
                .unwrap();
        assert_eq!(
            ed_20240105.metadosis_state,
            outbe_emission::schema::metadosis_state::COMPLETED,
            "A7 must settle 20240105 once A6 has sealed WWD 20240101"
        );
    });

    // Also verify end-to-end that `PrecompileError::Revert(String)` inside A6 is caught,
    // rolled back, and ABI-encoded as Solidity `Error(string)` in `WorldwideDayAdvanceFailed.reason`.
    // At 2024-01-07 00:00 UTC, grant 1 mutation frame so A4 (`dispatch_terminal_remainder_at`)
    // succeeds and A6 (`advance_worldwide_days_command`) hits `PrecompileError::Revert(String)`.
    block_no += 1;
    let ts_day7_00h = GENESIS_TS + 6 * SECONDS_PER_DAY;
    storage.set_block_number(block_no);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 1);
    storage.enter(|handle| {
        let block = BlockContext::new(block_no, ts_day7_00h, CHAIN_ID, Address::ZERO, Vec::new())
            .with_genesis_timestamp(GENESIS_TS + 60);
        let ctx = runtime_context(block, handle);
        account_parent(&ctx, block_no);
        // Settle previous day (A4) consumes the 1 frame; A6 reverts with `PrecompileError::Revert(String)`.
        crate::handler::run_protocol_cycle(&ctx).expect_err("A7 has 0 frames left after A6");
    });
    let revert_str_event = storage
        .get_events(TRIBUTE_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ITributeFactory::WorldwideDayAdvanceFailed::decode_log_data(log).ok())
        .find(|ev| ev.utcDay == 20_240_107)
        .expect("WorldwideDayAdvanceFailed must be emitted when A6 reverts with Revert(String)");
    let decoded = alloy_sol_types::Revert::abi_decode(&revert_str_event.reason)
        .expect("Revert(String) reason must be ABI-encoded as Error(string)");
    assert_eq!(
        decoded.reason, "test provider has no matching Metadosis mutation lease",
        "unexpected decoded Error(string): {}",
        decoded.reason
    );
}
