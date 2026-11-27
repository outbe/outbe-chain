//! OIP-00343 proof tests for `ED01`, `ED03`, and `ED54`.

use alloy_primitives::U256;
use outbe_emission::day_emission::emission_limit_of_day;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{error::PrecompileError, storage::MutationPurposeTag};
use outbe_system_common::address_pair::AddressPair;
use outbe_system_common::addresses::WORLDWIDE_DAY_ADDRESS;
use outbe_system_common::time::{
    date_key_to_utc_timestamp, day_number_between, timestamp_to_date_key, WorldwideDay,
    SECONDS_PER_DAY,
};
use outbe_worldwideday::WorldwideDayContract;

use super::*;

#[test]
fn every_utc_day_is_an_emission_day() {
    // Genesis at 2026-10-01 00:01:00 UTC (date key 20261001).
    // The first WWD is 20260930 (forming_end = 2026-10-01 12:00 UTC > genesis), whose E = D+1 is 20261001.
    // Each day E from 20261001 has wwd == E-1, with 0 Nods.
    let genesis_oct_1 = date_key_to_utc_timestamp(20_261_001).unwrap() + 60;
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);

    storage.enter(|handle| {
        let block = BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 1,
            timestamp: genesis_oct_1,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: genesis_oct_1,
            proposer: Address::ZERO,
            validators: Vec::new(),
        };
        let ctx = runtime_context(block, handle);
        run_cycle_tick(&ctx).unwrap();
    });

    // Advance hour by hour across 4 full UTC days (20261001..=20261004) into 20261005 13:00 UTC,
    // after the Metadosis of 20261004, with zero Nods and zero oracle prices.
    let first_hour = (genesis_oct_1 / 3_600 + 1) * 3_600;
    let end_ts = date_key_to_utc_timestamp(20_261_005).unwrap() + 13 * 3_600;
    let mut block_number = 2u64;
    let mut ts = first_hour;
    while ts <= end_ts {
        storage.set_block_number(block_number);
        storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
        storage.enter(|handle| {
            let block = BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number,
                timestamp: ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: genesis_oct_1,
                proposer: Address::ZERO,
                validators: Vec::new(),
            };
            let ctx = runtime_context(block, handle);
            account_parent(&ctx, block_number);
            run_due_tasks(&ctx).unwrap();
            crate::handler::process_agent_rewards(&ctx).unwrap();
        });
        block_number += 1;
        ts += 3_600;
    }

    storage.enter(|handle| {
        let expected_days = [
            (20_261_001u32, 0u32, WorldwideDay::new(20_260_930)),
            (20_261_002u32, 1u32, WorldwideDay::new(20_261_001)),
            (20_261_003u32, 2u32, WorldwideDay::new(20_261_002)),
            (20_261_004u32, 3u32, WorldwideDay::new(20_261_003)),
        ];
        let mut expected_promis_total = U256::ZERO;
        for (day, day_index, expected_wwd) in expected_days {
            let record = outbe_emission::api::get_emission_day(handle.clone(), day)
                .unwrap()
                .unwrap_or_else(|| panic!("missing EmissionDay record for {day}"));
            assert_eq!(record.wwd, expected_wwd, "unexpected wwd for day {day}");
            let limit = emission_limit_of_day(day_index);
            assert_eq!(
                U256::from(record.emission_limit),
                limit,
                "day {day} must receive emission_limit_of_day({day_index})"
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
                "distributed pools + PromisLimit must conserve emission_limit_of_day({day_index}) on day {day}"
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
fn start_date_comes_from_genesis_block() {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 32);

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
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 2,
                timestamp: block_ts,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 0,
                proposer: Address::ZERO,
                validators: Vec::new(),
            },
            handle.clone(),
        );
        let err = crate::handler::process_emission_day(&ctx_zero_genesis)
            .expect_err("zero genesis_timestamp must fail");
        assert!(matches!(err, PrecompileError::Fatal(_)));
    });

    // 3. Cycle runs from block 1 and settles day 0 at the next UTC midnight without any
    //    pre-seeded Rewards start_date slot.
    storage.enter(|handle| {
        let block_1 = runtime_context(block_ctx(1, GENESIS_TS + 60), handle);
        run_cycle_tick(&block_1).unwrap();
    });
    storage.set_block_number(2);
    storage.enter(|handle| {
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 3_600;
        let ctx = dispatch_at(handle.clone(), 2, fire_ts);
        crate::handler::process_agent_rewards(&ctx).unwrap();
        let record = outbe_emission::api::get_emission_day(handle, 20_240_101)
            .unwrap()
            .expect("genesis day must be allocated");
        assert_eq!(U256::from(record.emission_limit), emission_limit_of_day(0));
    });
}

#[test]
fn wwd_step_runs_once_a_day() {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 64);

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
        let wwd_20240103_start = WorldwideDay::new(20_240_103).start_timestamp().unwrap();
        oracle
            .write_snapshot(
                wwd_20240103_start + 3_600,
                &[(AddressPair::new_coen_to(840), U256::from(100), U256::ONE)],
            )
            .unwrap();

        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
        let factory = WorldwideDayContract::new(handle);
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
        let factory = WorldwideDayContract::new(ctx.storage.clone());
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
    // - The daily step A6 stores no price.
    // - WWD D closes/seals at 00:00 UTC of D+3, so on 2024-01-05 00:00 UTC (day_offset = 4) 20240103..=20240106
    //   remain.
    let mut block_no = 3u64;
    for day_offset in 1..=4u64 {
        let midnight_ts = GENESIS_TS + day_offset * SECONDS_PER_DAY;
        storage.set_block_number(block_no);
        storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
        storage.enter(|handle| {
            let ctx = dispatch_at(handle.clone(), block_no, midnight_ts);
            crate::handler::process_agent_rewards(&ctx).unwrap();
            let expected_day = timestamp_to_date_key(midnight_ts);
            let factory = WorldwideDayContract::new(ctx.storage.clone());
            assert_eq!(factory.last_wwd_advance_day.read().unwrap(), expected_day);
        });
        block_no += 1;
    }

    // Verify exact offering window boundaries [2024-01-03 10:00 UTC, 2024-01-05 12:00 UTC) for WWD 20240103:
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
                outbe_worldwideday::is_offering_day(handle.clone(), WorldwideDay::new(20_240_103))
                    .unwrap(),
                expected,
                "unexpected is_offering_day at ts={ts}"
            );
        }
    });

    // Run all intraday hourly ticks 01:00..=23:00 UTC on 2024-01-05:
    // - No new WorldwideDayCreated or WorldwideDayClosed events are emitted at any intraday hour.
    // - At 13:00 UTC (after WWD 20240103 offering_end = 12:00 UTC and after WWD 20240104 forming_end = 12:00 UTC):
    //   * `is_offering_day(20240103)` is false by block timestamp (`now >= offering_end`).
    //   * `is_day_sealed(20240103)` is still false and 20240103 remains in `pipeline_wwd`.
    let events_before_intraday = storage.get_events(WORLDWIDE_DAY_ADDRESS).len();
    for hour in 1..=23u64 {
        let ts_hour = GENESIS_TS + 4 * SECONDS_PER_DAY + hour * 3_600;
        storage.set_block_number(block_no);
        storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
        storage.enter(|handle| {
            let ctx = dispatch_at(handle.clone(), block_no, ts_hour);
            crate::handler::process_agent_rewards(&ctx).unwrap();
            let factory = WorldwideDayContract::new(ctx.storage.clone());
            assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_105);
            if hour == 13 {
                assert!(!outbe_worldwideday::is_offering_day(
                    ctx.storage.clone(),
                    WorldwideDay::new(20_240_103)
                )
                .unwrap());
                assert!(
                    !outbe_worldwideday::is_worldwide_day_sealed(
                        ctx.storage.clone(),
                        WorldwideDay::new(20_240_103)
                    )
                    .unwrap(),
                    "WWD 20240103 must not be sealed at 13:00 UTC; A6 seals at 00:00 UTC"
                );
                assert!(
                    factory
                        .pipeline_wwd
                        .read_all()
                        .unwrap()
                        .contains(&WorldwideDay::new(20_240_103)),
                    "WWD 20240103 must remain in pipeline at 13:00 UTC; A6 runs at 00:00 UTC"
                );
            }
        });
        block_no += 1;
    }
    assert_eq!(
        storage.get_events(WORLDWIDE_DAY_ADDRESS).len(),
        events_before_intraday,
        "intraday ticks 01:00..=23:00 UTC must not emit duplicate Worldwide Day events"
    );

    // Force an A6 revert at 2024-01-06 00:00 UTC by pre-linking future EmissionDay 20240108
    // (the E = D+1 target of WWD 20240107) to a conflicting WWD, so creating WWD 20240107 inside
    // `advance_worldwide_days` reverts with `RevertBytes`.
    // Verify:
    // - Step A4 (`allocate_emission_limit(20240105)`) still commits (`state == PROCESSING`), and
    //   the Metadosis of 20240105 waits for 12:00 UTC.
    // - Step A6 rolls back its inner checkpoint (`last_wwd_advance_day` stays 20240105, WWD 20240103
    //   remains unsealed in `pipeline_wwd`) and emits no event.
    // - The Metadosis of 20240104 ran at 12:00 UTC on 2024-01-05, before the seal of its WWD.
    storage.enter(|handle| {
        outbe_emission::api::link_wwd(handle, 20_240_108, WorldwideDay::new(20_240_120)).unwrap();
    });
    let ts_day6_00h = GENESIS_TS + 5 * SECONDS_PER_DAY;
    let factory_events_before_failed_a6 = storage.get_events(WORLDWIDE_DAY_ADDRESS).len();
    storage.set_block_number(block_no);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), block_no, ts_day6_00h);
        let factory = WorldwideDayContract::new(ctx.storage.clone());
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
                .contains(&WorldwideDay::new(20_240_103)),
            "reverted A6 must roll back closing/sealing of WWD 20240103"
        );
        assert!(
            factory
                .worldwide_days
                .get(WorldwideDay::new(20_240_107))
                .unwrap()
                .is_none(),
            "reverted A6 must roll back the creation of WWD 20240107"
        );
        let ed_20240105 = outbe_emission::api::get_emission_day(ctx.storage.clone(), 20_240_105)
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
            "the Metadosis of 20240105 starts at 12:00 UTC"
        );
        let ed_20240104 = outbe_emission::api::get_emission_day(ctx.storage.clone(), 20_240_104)
            .unwrap()
            .unwrap();
        assert_eq!(
            ed_20240104.metadosis_state,
            outbe_emission::schema::metadosis_state::COMPLETED,
            "the Metadosis of 20240104 runs before the seal of WWD 20240103"
        );
    });
    assert_eq!(
        storage.get_events(WORLDWIDE_DAY_ADDRESS).len(),
        factory_events_before_failed_a6,
        "a reverted A6 must emit no Worldwide Day event"
    );
    block_no += 1;

    // Clear the conflicting pre-link on 20240108 and advance to the next hourly tick (2024-01-06 01:00 UTC).
    // Now A6 retries, closes and seals WWD 20240103 that Metadosis already retired, finalizes the
    // forming snapshot of WWD 20240104, creates WWD 20240107 and updates `last_wwd_advance_day = 20240106`.
    storage.enter(|handle| {
        let mut clear = outbe_emission::api::get_emission_day(handle.clone(), 20_240_108)
            .unwrap()
            .unwrap();
        clear.wwd = WorldwideDay::new(0);
        outbe_emission::api::put_emission_day(handle, &clear).unwrap();
    });
    let ts_day6_01h = ts_day6_00h + 3_600;
    storage.set_block_number(block_no);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), block_no, ts_day6_01h);
        crate::handler::process_agent_rewards(&ctx).unwrap();
        let factory = WorldwideDayContract::new(ctx.storage.clone());
        assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_106);
        assert!(
            !factory
                .pipeline_wwd
                .read_all()
                .unwrap()
                .contains(&WorldwideDay::new(20_240_103)),
            "A6 retry at 01:00 UTC must close WWD 20240103"
        );
        assert!(
            outbe_worldwideday::is_worldwide_day_sealed(
                ctx.storage.clone(),
                WorldwideDay::new(20_240_103)
            )
            .unwrap(),
            "A6 retry at 01:00 UTC must seal WWD 20240103"
        );
        assert!(
            factory
                .worldwide_days
                .get(WorldwideDay::new(20_240_107))
                .unwrap()
                .is_some(),
            "A6 retry at 01:00 UTC must create WWD X+1 (20240107)"
        );
        let ed_20240104 = outbe_emission::api::get_emission_day(ctx.storage.clone(), 20_240_104)
            .unwrap()
            .unwrap();
        assert_eq!(
            ed_20240104.metadosis_state,
            outbe_emission::schema::metadosis_state::COMPLETED,
            "the Metadosis of 20240104 stays completed"
        );
    });

    // Also verify that `PrecompileError::Revert(String)` inside A6 is caught and emits no event.
    // At 2024-01-07 00:00 UTC, A6 runs with no mutation frame and hits `PrecompileError::Revert(String)`.
    block_no += 1;
    let ts_day7_00h = GENESIS_TS + 6 * SECONDS_PER_DAY;
    let factory_events_before_text_revert = storage.get_events(WORLDWIDE_DAY_ADDRESS).len();
    storage.set_block_number(block_no);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 0);
    storage.enter(|handle| {
        let block = BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: block_no,
            timestamp: ts_day7_00h,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: GENESIS_TS + 60,
            proposer: Address::ZERO,
            validators: Vec::new(),
        };
        let ctx = runtime_context(block, handle);
        crate::handler::run_advance_worldwide_days_step(&ctx, 20_240_107).unwrap();
        let factory = WorldwideDayContract::new(ctx.storage.clone());
        assert_eq!(factory.last_wwd_advance_day.read().unwrap(), 20_240_106);
        assert!(factory
            .worldwide_days
            .get(WorldwideDay::new(20_240_108))
            .unwrap()
            .is_none());
    });
    assert_eq!(
        storage.get_events(WORLDWIDE_DAY_ADDRESS).len(),
        factory_events_before_text_revert,
        "a text revert of A6 must emit no Worldwide Day event"
    );
}

#[test]
fn metadosis_starts_at_the_offering_end() {
    use outbe_emission::schema::metadosis_state;
    let metadosis_state_of = |handle: &StorageHandle<'_>, day: u32| {
        outbe_emission::api::get_emission_day(handle.clone(), day)
            .unwrap()
            .unwrap()
            .metadosis_state
    };
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 64);
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
        let wwd = outbe_worldwideday::api::get_worldwide_day(
            handle.clone(),
            WorldwideDay::new(20_231_231),
        )
        .unwrap()
        .unwrap();
        let day = outbe_emission::api::get_emission_day(handle, 20_240_101)
            .unwrap()
            .unwrap();
        assert_eq!(wwd.emission_day, 20_240_101);
        assert_eq!(day.metadosis_start, wwd.offering_end);
        assert_eq!(wwd.offering_end, GENESIS_TS + SECONDS_PER_DAY + 12 * 3_600);
    });

    // The allocation of 20240101 runs at 00:00 UTC on 2024-01-02, and its Metadosis waits for 12:00 UTC.
    for (block_number, timestamp, expected) in [
        (
            2,
            GENESIS_TS + SECONDS_PER_DAY + 1,
            metadosis_state::PENDING,
        ),
        (
            3,
            GENESIS_TS + SECONDS_PER_DAY + 11 * 3_600 + 1,
            metadosis_state::PENDING,
        ),
        (
            4,
            GENESIS_TS + SECONDS_PER_DAY + 12 * 3_600 + 1,
            metadosis_state::COMPLETED,
        ),
    ] {
        storage.set_block_number(block_number);
        storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
        storage.enter(|handle| {
            let ctx = dispatch_at(handle.clone(), block_number, timestamp);
            assert!(is_day_allocated(&ctx, 20_240_101));
            assert_eq!(metadosis_state_of(&handle, 20_240_101), expected);
        });
    }

    // 20240102 is allocated, then the chain stops until 2024-01-04 01:00 UTC, when the votes of
    // 20240103 are still open: the due Metadosis of 20240102 runs, and 20240103 waits.
    storage.set_block_number(5);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 5, GENESIS_TS + 2 * SECONDS_PER_DAY + 1);
        assert!(is_day_allocated(&ctx, 20_240_102));
        assert_eq!(
            metadosis_state_of(&handle, 20_240_102),
            metadosis_state::PENDING
        );
    });
    storage.set_block_number(6);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        handle
            .contract::<outbe_rewards::schema::Rewards>()
            .day_votes_sealed_at_block
            .write(&20_240_103, 6)
            .unwrap();
        let ctx = dispatch_at(
            handle.clone(),
            6,
            GENESIS_TS + 3 * SECONDS_PER_DAY + 3_600 + 1,
        );
        assert_eq!(
            metadosis_state_of(&handle, 20_240_102),
            metadosis_state::COMPLETED
        );
        assert!(!is_day_allocated(&ctx, 20_240_103));
    });
}

const NOT_ALLOCATED: Option<u8> = None;

/// One second after `hour`:00 UTC on 2024-01-`day`.
fn jan(day: u64, hour: u64) -> u64 {
    GENESIS_TS + (day - 1) * SECONDS_PER_DAY + hour * 3_600 + 1
}

fn started_cycle() -> HashMapStorageProvider {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 64);
    storage.enter(|handle| {
        run_cycle_tick(&genesis_block(handle, GENESIS_TS + 60)).unwrap();
    });
    storage
}

/// A block number, its timestamp, and each checked day with its expected state.
type CheckedBlock<'a> = (u64, u64, &'a [(u32, Option<u8>)]);

/// Runs one block per entry, then checks each listed day: `None` when it is not allocated,
/// else its Metadosis state.
fn run_blocks(storage: &mut HashMapStorageProvider, blocks: &[CheckedBlock<'_>]) {
    for &(block_number, timestamp, expected) in blocks {
        storage.set_block_number(block_number);
        storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
        storage.enter(|handle| {
            let ctx = dispatch_at(handle.clone(), block_number, timestamp);
            for &(day, state) in expected {
                let actual = is_day_allocated(&ctx, day).then(|| {
                    outbe_emission::api::get_emission_day(handle.clone(), day)
                        .unwrap()
                        .unwrap()
                        .metadosis_state
                });
                assert_eq!(actual, state, "day {day} after block {block_number}");
            }
        });
    }
}

#[test]
fn a_stop_of_three_days_gives_each_day_its_allocation_and_metadosis_in_one_block() {
    use outbe_emission::schema::metadosis_state::COMPLETED;
    let mut storage = started_cycle();
    // The chain stops after the Metadosis of 20240101 and runs again at 13:00 UTC on 2024-01-05.
    run_blocks(
        &mut storage,
        &[
            (2, jan(2, 12), &[(20_240_101, Some(COMPLETED))]),
            (
                3,
                jan(5, 13),
                &[(20_240_102, Some(COMPLETED)), (20_240_103, NOT_ALLOCATED)],
            ),
            (
                4,
                jan(5, 14),
                &[(20_240_103, Some(COMPLETED)), (20_240_104, NOT_ALLOCATED)],
            ),
            (
                5,
                jan(5, 15),
                &[(20_240_104, Some(COMPLETED)), (20_240_105, NOT_ALLOCATED)],
            ),
            (6, jan(6, 12), &[(20_240_105, Some(COMPLETED))]),
        ],
    );
}

#[test]
fn a_stop_between_the_allocation_and_its_metadosis_runs_each_metadosis_one_run_later() {
    use outbe_emission::schema::metadosis_state::{COMPLETED, PENDING};
    let mut storage = started_cycle();
    // The chain stops after the allocation of 20240101 at 00:00 UTC on 2024-01-02, before its
    // Metadosis, and runs again at 13:00 UTC on 2024-01-05.
    run_blocks(
        &mut storage,
        &[
            (2, jan(2, 0), &[(20_240_101, Some(PENDING))]),
            (
                3,
                jan(5, 13),
                &[(20_240_101, Some(COMPLETED)), (20_240_102, Some(PENDING))],
            ),
            (
                4,
                jan(5, 14),
                &[(20_240_102, Some(COMPLETED)), (20_240_103, Some(PENDING))],
            ),
            (
                5,
                jan(5, 15),
                &[(20_240_103, Some(COMPLETED)), (20_240_104, Some(PENDING))],
            ),
            (
                6,
                jan(5, 16),
                &[(20_240_104, Some(COMPLETED)), (20_240_105, NOT_ALLOCATED)],
            ),
            (7, jan(6, 12), &[(20_240_105, Some(COMPLETED))]),
        ],
    );
}

#[test]
fn the_auction_starts_live_at_00_00_and_takes_its_brief_at_12_00() {
    use alloy_sol_types::SolEvent;
    use outbe_desis::precompile::IDesis;
    use outbe_desis::schema::{AuctionStage, DesisContract};
    use outbe_system_common::addresses::DESIS_ADDRESS;
    let day = outbe_system_common::time::EmissionDate::new(20_240_101);
    let mut storage = started_cycle();
    storage.enter(|handle| {
        let (_, index) = outbe_oracle::api::require_coen_pair(handle.clone(), 840).unwrap();
        let oracle = OracleContract::new(handle);
        for priced in [20_231_231, 20_240_101] {
            oracle
                .record_utc_day_vwap(priced, index, U256::from(2_000_000u64))
                .unwrap();
        }
    });
    let desis_events = |storage: &HashMapStorageProvider| {
        let logs = storage.get_events(DESIS_ADDRESS);
        let limits: Vec<_> = logs
            .iter()
            .filter_map(|log| IDesis::DesisLimitSet::decode_log_data(log).ok())
            .map(|ev| (ev.emissionDay, ev.amountMinor))
            .collect();
        let cancels: Vec<_> = logs
            .iter()
            .filter_map(|log| IDesis::DesisCanceled::decode_log_data(log).ok())
            .map(|ev| (ev.emissionDay, ev.reason))
            .collect();
        (limits, cancels)
    };

    storage.set_block_number(2);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        dispatch_at(handle.clone(), 2, jan(2, 0));
        let desis = DesisContract::new(handle);
        assert_eq!(
            desis.auction_stage.read(&day).unwrap(),
            AuctionStage::CommittingBids as u8
        );
        assert_eq!(desis.desis_limit_set.read(&day).unwrap(), 0);
    });
    assert_eq!(desis_events(&storage), (vec![], vec![]));

    // The day has no Nod, so its brief at 12:00 carries a zero limit and cancels the started
    // auction.
    storage.set_block_number(3);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 16);
    storage.enter(|handle| {
        dispatch_at(handle.clone(), 3, jan(2, 12));
        assert_eq!(
            DesisContract::new(handle).auction_stage.read(&day).unwrap(),
            AuctionStage::Canceled as u8
        );
    });
    assert_eq!(
        desis_events(&storage),
        (
            vec![(20_240_101, U256::ZERO)],
            vec![(20_240_101, outbe_desis::constants::CANCELED_NO_LIMIT)]
        )
    );
}
