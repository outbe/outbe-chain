//! OIP-00343 proof tests for `ED01`, `ED03`, and `ED54`.

use alloy_primitives::U256;
use outbe_emission::day_emission::emission_limit_of_day;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{error::PrecompileError, storage::MutationPurposeTag};
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
        dispatch_at(handle.clone(), 2, fire_ts);
        let record = outbe_emission::api::get_emission_day(handle, 20_240_101)
            .unwrap()
            .expect("genesis day must be allocated");
        assert_eq!(U256::from(record.emission_limit), emission_limit_of_day(0));
    });
}

#[test]
fn wwd_step_opens_the_wwd_of_the_day_and_seals_it_at_the_offering_end() {
    let mut storage = cycle_storage();
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 64);
    let pipeline = |ctx: &BlockRuntimeContext<'_>| {
        WorldwideDayContract::new(ctx.storage.clone())
            .pipeline_wwd
            .read_all()
            .unwrap()
    };
    let last_day = |ctx: &BlockRuntimeContext<'_>| {
        WorldwideDayContract::new(ctx.storage.clone())
            .last_wwd_advance_day
            .read()
            .unwrap()
    };
    let wwd = WorldwideDay::new;

    // The first block opens the first WWD of the genesis and the WWD of the day, with the
    // records of their emission days.
    storage.enter(|handle| {
        let anchor = genesis_block(handle.clone(), GENESIS_TS + 60);
        run_cycle_tick(&anchor).unwrap();
        assert_eq!(last_day(&anchor), 20_240_101);
        assert_eq!(pipeline(&anchor), vec![wwd(20_231_231), wwd(20_240_101)]);
        for (day, linked) in [(20_240_101, 20_231_231), (20_240_102, 20_240_101)] {
            let record = outbe_emission::api::get_emission_day(handle.clone(), day)
                .unwrap()
                .unwrap();
            assert_eq!(record.wwd, wwd(linked));
            assert_ne!(record.metadosis_limit, 0, "the record has its limits");
        }
    });

    // An hourly run on the same day opens nothing.
    storage.set_block_number(2);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle, 2, GENESIS_TS + 13 * 3_600);
        assert_eq!(last_day(&ctx), 20_240_101);
        assert_eq!(pipeline(&ctx), vec![wwd(20_231_231), wwd(20_240_101)]);
    });

    // 00:00 of 2024-01-02 opens WWD 20240102; its offering ends at 12:00 of 2024-01-04.
    storage.set_block_number(3);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 3, GENESIS_TS + SECONDS_PER_DAY + 1);
        assert_eq!(last_day(&ctx), 20_240_102);
        assert_eq!(
            pipeline(&ctx),
            vec![wwd(20_231_231), wwd(20_240_101), wwd(20_240_102)]
        );
        assert_eq!(
            outbe_emission::api::get_emission_day(handle, 20_240_103)
                .unwrap()
                .unwrap()
                .wwd,
            wwd(20_240_102)
        );
    });

    // The run at 12:00 of 2024-01-02, when the offering of WWD 20231231 ends, seals it.
    storage.set_block_number(4);
    storage.enter(|handle| {
        let ctx = dispatch_at(
            handle.clone(),
            4,
            GENESIS_TS + SECONDS_PER_DAY + 12 * 3_600 + 1,
        );
        assert_eq!(pipeline(&ctx), vec![wwd(20_240_101), wwd(20_240_102)]);
        assert!(outbe_worldwideday::is_worldwide_day_sealed(handle, wwd(20_231_231)).unwrap());
    });

    // A record of 20240104 that names another WWD makes the step of 2024-01-03 revert: the WWD of
    // the day is not opened, and no WWD event is emitted. The day start still commits.
    storage.enter(|handle| {
        let mut conflict = outbe_emission::api::get_emission_day(handle.clone(), 20_240_103)
            .unwrap()
            .unwrap();
        conflict.day = 20_240_104;
        conflict.wwd = wwd(20_240_120);
        outbe_emission::api::put_emission_day(handle, &conflict).unwrap();
    });
    let events_before = storage.get_events(WORLDWIDE_DAY_ADDRESS).len();
    storage.set_block_number(5);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 5, GENESIS_TS + 2 * SECONDS_PER_DAY + 1);
        assert_eq!(last_day(&ctx), 20_240_102);
        assert!(WorldwideDayContract::new(handle.clone())
            .worldwide_days
            .get(wwd(20_240_103))
            .unwrap()
            .is_none());
        assert_eq!(
            outbe_emission::api::get_emission_day(handle, 20_240_102)
                .unwrap()
                .unwrap()
                .state,
            outbe_emission::schema::emission_day_state::PROCESSING
        );
    });
    assert_eq!(
        storage.get_events(WORLDWIDE_DAY_ADDRESS).len(),
        events_before
    );

    // With the conflict gone, the next hourly run opens the WWD of the day.
    storage.enter(|handle| {
        outbe_emission::api::delete_emission_day(handle, 20_240_104).unwrap();
    });
    storage.set_block_number(6);
    storage.enter(|handle| {
        let ctx = dispatch_at(handle.clone(), 6, GENESIS_TS + 2 * SECONDS_PER_DAY + 3_601);
        assert_eq!(last_day(&ctx), 20_240_103);
        assert_eq!(
            outbe_emission::api::get_emission_day(handle, 20_240_104)
                .unwrap()
                .unwrap()
                .wwd,
            wwd(20_240_103)
        );
    });

    // A text revert inside the step is caught and emits no event.
    let events_before = storage.get_events(WORLDWIDE_DAY_ADDRESS).len();
    storage.set_block_number(7);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 0);
    storage.enter(|handle| {
        let ctx = runtime_context(block_ctx(7, GENESIS_TS + 3 * SECONDS_PER_DAY), handle);
        crate::handler::run_advance_worldwide_days_step(&ctx, 20_240_104).unwrap();
        assert_eq!(last_day(&ctx), 20_240_103);
        assert!(WorldwideDayContract::new(ctx.storage.clone())
            .worldwide_days
            .get(wwd(20_240_104))
            .unwrap()
            .is_none());
    });
    assert_eq!(
        storage.get_events(WORLDWIDE_DAY_ADDRESS).len(),
        events_before
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
