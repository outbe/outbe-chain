//! Lifecycle tests: tally, begin-block hooks, slash window, OCOMP atomicity.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::Units;

use crate::schema::{OracleContract, SCALE_1E18};

use super::common::*;

#[test]
fn ocomp_pre_admission_selects_stored_price_and_reads_bounded_counts() {
    let timestamp = 1_753_315_200_u64;
    with_storage_at(timestamp, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair(COEN, usd()).unwrap();
        let pair = pair_key(COEN, usd());
        let wwd = outbe_common::WorldwideDay::from_timestamp(timestamp);
        let last_closed = outbe_primitives::time::previous_date_key(
            outbe_primitives::time::timestamp_to_date_key(timestamp),
        );
        let last_closed_start = outbe_primitives::time::date_key_to_utc_timestamp(last_closed);
        let last_closed_price = U256::from(125);

        let uninitialized = crate::api::ocomp_pre_admission_projection(
            storage.clone(),
            wwd,
            U256::from(99),
            timestamp,
        )
        .unwrap();
        assert!(!uninitialized.profile_ready);
        assert_eq!(uninitialized.oracle_state_version, 0);

        crate::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();
        oracle
            .write_snapshot(
                last_closed_start + 100,
                &[(pair, last_closed_price, U256::from(1))],
            )
            .unwrap();
        oracle
            .store_worldwide_day_vwap_snapshot(
                wwd,
                last_closed_start,
                last_closed_start + outbe_primitives::time::SECONDS_PER_DAY,
            )
            .unwrap();
        oracle.finalize_utc_day_vwap(last_closed).unwrap();
        crate::scurve::store_scurve_entry(&mut oracle, pair_id, last_closed_start, U256::from(200))
            .unwrap();

        let closed = crate::api::ocomp_pre_admission_projection(
            storage.clone(),
            wwd,
            U256::from(99),
            timestamp,
        )
        .unwrap();
        assert!(closed.profile_ready);
        assert_eq!(closed.auction_entry_price, last_closed_price);
        assert_eq!(
            closed.auction_entry_price_source,
            crate::api::OcompAuctionEntryPriceSource::LastClosedDayVwap
        );
        assert_eq!(closed.auction_entry_price_source_day, last_closed);
        assert_eq!(closed.oracle_state_version, 5);
        assert_eq!(closed.wwd_pair_entries, 1);
        assert_eq!(closed.active_scurve_entries, 1);

        let next_timestamp = timestamp + outbe_primitives::time::SECONDS_PER_DAY;
        let next_wwd = outbe_common::WorldwideDay::from_timestamp(next_timestamp);
        let fallback = crate::api::ocomp_pre_admission_projection(
            storage,
            next_wwd,
            U256::from(99),
            next_timestamp,
        )
        .unwrap();
        assert!(fallback.profile_ready);
        assert_eq!(fallback.auction_entry_price, U256::from(99));
        assert_eq!(
            fallback.auction_entry_price_source,
            crate::api::OcompAuctionEntryPriceSource::CurrentVwapFallback
        );
        assert_eq!(fallback.auction_entry_price_source_day, next_wwd.value());
        assert_eq!(fallback.oracle_state_version, 5);
        assert_eq!(fallback.wwd_pair_entries, 0);
        assert_eq!(fallback.active_scurve_entries, 1);
    });
}

#[test]
fn ocomp_oracle_profile_initialization_is_exact_and_idempotent() {
    with_storage(|storage| {
        assert!(crate::api::initialize_fresh_ocomp_profile(storage.clone()).is_err());

        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair(COEN, usd()).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage.clone()).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();

        assert!(oracle.ocomp_profile_ready.read().unwrap());
        assert_eq!(oracle.ocomp_day_type_pair_id.read().unwrap(), pair_id);
        assert_eq!(oracle.ocomp_state_version.read().unwrap(), 1);
    });
}

#[test]
fn ocomp_state_version_overflow_rejects_before_oracle_mutation() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(COEN, usd()).unwrap();
        let pair = pair_key(COEN, usd());
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();
        oracle.ocomp_state_version.write(u64::MAX).unwrap();

        assert!(oracle
            .write_snapshot(1_000, &[(pair, U256::from(10), U256::from(1))],)
            .is_err());
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_eq!(oracle.snapshot_pair_count.read(&0).unwrap(), 0);
        assert_eq!(oracle.ocomp_state_version.read().unwrap(), u64::MAX);
    });
}
#[test]
fn ocomp_oracle_owner_mutations_roll_back_every_partial_write_boundary() {
    for (label, fixture, mutation) in [
        (
            "write_snapshot",
            seed_ocomp_oracle as OracleFixture,
            write_snapshot_mutation as OracleMutation,
        ),
        (
            "store_worldwide_day_vwap_snapshot",
            seed_ocomp_oracle_with_snapshot,
            store_wwd_snapshot_mutation,
        ),
        (
            "finalize_utc_day_vwap",
            seed_ocomp_oracle_with_snapshot,
            finalize_utc_day_mutation,
        ),
        (
            "store_scurve_entry",
            seed_ocomp_oracle,
            store_scurve_mutation,
        ),
        (
            "evict_expired_scurves",
            seed_ocomp_oracle_with_scurve,
            evict_scurve_mutation,
        ),
        (
            "process_daily_scurve",
            seed_ocomp_oracle_with_peak_history,
            process_scurve_mutation,
        ),
    ] {
        assert_oracle_mutation_is_atomic(label, fixture, mutation);
    }
}
#[test]
fn prefork_oracle_event_failures_preserve_historical_best_effort_mutations() {
    let mut finalized = run_prefork_with_last_mutation_failure(
        seed_prefork_oracle_with_snapshot,
        finalize_utc_day_mutation,
    );
    StorageHandle::enter(&mut finalized, |storage| {
        let oracle = OracleContract::new(storage);
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair_id(
                    outbe_primitives::time::timestamp_to_date_key(ATOMIC_DAY_START),
                    1,
                )
                .unwrap(),
            Some(U256::from(125))
        );
        assert!(!oracle.ocomp_profile_ready.read().unwrap());
    });

    let mut processed = run_prefork_with_last_mutation_failure(
        seed_prefork_oracle_with_peak_history,
        process_scurve_mutation,
    );
    StorageHandle::enter(&mut processed, |storage| {
        let oracle = OracleContract::new(storage);
        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert_eq!(
            oracle.scurve_peak_day.read(&0).unwrap(),
            SCURVE_CURRENT_DAY - 2 * crate::scurve::DAY_SECONDS
        );
        assert!(!oracle.ocomp_profile_ready.read().unwrap());
    });
}

#[test]
fn scurve_count_overflow_rejects_before_any_owner_write() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(COEN, usd()).unwrap();
        crate::api::initialize_fresh_ocomp_profile(storage).unwrap();
        oracle.scurve_count.write(u32::MAX).unwrap();
        let version_before = oracle.ocomp_state_version.read().unwrap();

        assert!(crate::scurve::store_scurve_entry(
            &mut oracle,
            1,
            ATOMIC_DAY_START,
            U256::from(125),
        )
        .is_err());
        assert_eq!(oracle.scurve_count.read().unwrap(), u32::MAX);
        assert_eq!(oracle.scurve_pair_id.read(&u32::MAX).unwrap(), 0);
        assert_eq!(oracle.ocomp_state_version.read().unwrap(), version_before);
    });
}
#[test]
fn run_tally_accepts_a_single_validator_as_the_weighted_median() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(COEN, USDT).unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, U256::in_units(100u64));
        let rate = U256::in_units(50u64);
        let volume = U256::in_units(1000u64);

        oracle
            .submit_vote(validator, &[(COEN, USDT, rate, volume)])
            .unwrap();

        // Run tally
        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        // Exchange rate should be updated to the voted rate
        let (stored_rate, block, ts) = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(stored_rate, rate);
        assert_eq!(block, 2);
        assert_eq!(ts, 24);

        // Validator should get success (voted within band for all pairs)
        assert_eq!(oracle.penalty_success_count.read(&validator).unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 0);
        assert_eq!(oracle.penalty_abstain_count.read(&validator).unwrap(), 0);

        // Votes should be cleared
        assert_eq!(oracle.voter_list.len().unwrap(), 0);

        // Snapshot should exist
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 1);
    });
}

#[test]
fn run_tally_rewards_every_voter_inside_the_reward_band() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(COEN, USDT).unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);

        register_validator(storage.clone(), v1, U256::in_units(100u64));
        register_validator(storage.clone(), v2, U256::in_units(200u64));
        register_validator(storage.clone(), v3, U256::in_units(100u64));

        // All vote very close: 1000, 1001, 1002 (spread < 0.2% of median)
        // With 2% reward band, all should be within band.
        let base = U256::in_units(1000u64);
        oracle
            .submit_vote(v1, &[(COEN, USDT, base, SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v2, &[(COEN, USDT, base + SCALE_1E18, SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v3, &[(COEN, USDT, base + U256::in_units(2u64), SCALE_1E18)])
            .unwrap();

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        // Weighted median: powers 100, 200, 100. Total=400, half=200.
        // Sorted: 1000(100), 1001(200), 1002(100).
        // Cumsum: 100(<200), 300(>=200) → median = 1001.
        let (rate, _, _) = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, U256::in_units(1001u64));

        // Reward spread = max(std_dev, 1001 * 0.02 / 2) = max(~0.816, ~10.01) = ~10.01
        // All votes within [990.99, 1011.01] → all win
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v3).unwrap(), 1);
    });
}

#[test]
fn run_tally_penalizes_a_voter_outside_the_reward_band() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(COEN, USDT).unwrap();

        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let v3 = Address::new([0x33; 20]);

        register_validator(storage.clone(), v1, U256::in_units(100u64));
        register_validator(storage.clone(), v2, U256::in_units(200u64));
        register_validator(storage.clone(), v3, U256::in_units(100u64));

        // v1 and v2 vote 50, v3 votes 500 (extreme outlier)
        oracle
            .submit_vote(v1, &[(COEN, USDT, U256::in_units(50u64), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v2, &[(COEN, USDT, U256::in_units(50u64), SCALE_1E18)])
            .unwrap();
        oracle
            .submit_vote(v3, &[(COEN, USDT, U256::in_units(500u64), SCALE_1E18)])
            .unwrap();

        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        // Median should be 50 (powers 100+200 cross threshold before 500)
        let (rate, _, _) = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, U256::in_units(50u64));

        // v1 and v2 should be winners, v3 (outlier at 500) should miss
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
        assert_eq!(oracle.penalty_miss_count.read(&v3).unwrap(), 1);
    });
}

#[test]
fn run_tally_counts_an_abstain_for_every_silent_validator() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(COEN, USDT).unwrap();

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, U256::in_units(100u64));

        // No votes submitted → all abstain
        crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

        assert_eq!(oracle.penalty_abstain_count.read(&v1).unwrap(), 1);
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 0);
    });
}

#[test]
fn begin_block_tallies_only_on_a_vote_period_boundary() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(COEN, USDT).unwrap();

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, U256::in_units(100u64));
        oracle
            .submit_vote(v1, &[(COEN, USDT, U256::in_units(42u64), SCALE_1E18)])
            .unwrap();

        // Block 1: not a vote period boundary (period=2), no tally
        let runtime_ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 12, 1), storage.clone());
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
        assert!(oracle.vote_exists.read(&v1).unwrap()); // vote still exists

        // Block 2: vote period boundary, tally runs
        let runtime_ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(2, 24, 1), storage.clone());
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
        assert!(!oracle.vote_exists.read(&v1).unwrap()); // votes cleared

        let (rate, _, _) = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, U256::in_units(42u64));
    });
}

#[test]
fn slash_window_resets_penalty_counters_at_the_window_end() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, U256::in_units(100u64));

        // Simulate many misses (below 5% success rate)
        for _ in 0..20 {
            oracle.increment_miss(&v1).unwrap();
        }
        oracle.increment_success(&v1).unwrap(); // 1 success out of 21 = 4.76% < 5%

        // Run slash and reset
        crate::tally::slash_and_reset_counters(&mut oracle, 10000).unwrap();

        // Counters should be reset
        assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 0);
        assert_eq!(oracle.penalty_miss_count.read(&v1).unwrap(), 0);

        // Validator should be force-exited (check via ValidatorSet)
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let info = vs.get_validator(v1).unwrap().unwrap();
        assert_eq!(info.status, outbe_validatorset::logic::status::JAILED);
    });
}

#[test]
fn slash_window_rejects_unbounded_validator_work() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);

        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_is_initialized.write(true).unwrap();
        vs.config_owner.write(Address::ZERO).unwrap();
        vs.config_epoch_length_blocks.write(3600).unwrap();
        vs.config_max_validators
            .write((crate::tally::MAX_ORACLE_SLASH_WINDOW_VALIDATORS + 1) as u32)
            .unwrap();

        for i in 1..=(crate::tally::MAX_ORACLE_SLASH_WINDOW_VALIDATORS + 1) {
            let mut bytes = [0u8; 20];
            bytes[16..].copy_from_slice(&(i as u32).to_be_bytes());
            register_validator(storage.clone(), Address::new(bytes), U256::from(1u64));
        }

        let err = crate::tally::slash_and_reset_counters(&mut oracle, 10_000).unwrap_err();
        assert!(
            err.to_string().contains("exceeds cap"),
            "unexpected error: {err}"
        );
    });
}

#[test]
fn slash_window_rolls_back_slash_state_when_force_exit_fails() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .config_slash_fraction
            .write(SCALE_1E18 / U256::from(10u64))
            .unwrap();

        let validator = Address::new([0x33; 20]);
        let stake = U256::in_units(100u64);
        register_validator(storage.clone(), validator, stake);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.pending_set_change.write(false).unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        oracle
            .storage
            .set_balance(outbe_primitives::addresses::STAKING_ADDRESS, stake)
            .unwrap();

        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.val_status
            .write(&validator, outbe_validatorset::logic::status::REGISTERED)
            .unwrap();

        oracle.increment_miss(&validator).unwrap();

        let err = crate::tally::slash_and_reset_counters(&mut oracle, 10_000).unwrap_err();
        assert!(err.to_string().contains("cannot jail validator"));

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        assert_eq!(staking.stake_amount.read(&validator).unwrap(), stake);
        assert_eq!(staking.total_staked.read().unwrap(), stake);
        assert_eq!(
            oracle
                .storage
                .balance(outbe_primitives::addresses::STAKING_ADDRESS)
                .unwrap(),
            stake
        );

        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert_eq!(vs.val_stake.read(&validator).unwrap(), stake);
        assert_eq!(
            vs.val_status.read(&validator).unwrap(),
            outbe_validatorset::logic::status::REGISTERED
        );

        assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 1);
    });
}

#[test]
fn slash_window_rolls_back_the_forced_exit_when_slashing_fails() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .config_slash_fraction
            .write(SCALE_1E18 / U256::from(10u64))
            .unwrap();

        let validator = Address::new([0x44; 20]);
        let stake = U256::in_units(100u64);
        register_validator(storage.clone(), validator, stake);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.pending_set_change.write(false).unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        oracle
            .storage
            .set_balance(outbe_primitives::addresses::STAKING_ADDRESS, U256::ZERO)
            .unwrap();

        oracle.increment_miss(&validator).unwrap();

        let err = crate::tally::slash_and_reset_counters(&mut oracle, 10_000).unwrap_err();
        assert!(
            err.to_string().contains("insufficient") || err.to_string().contains("balance"),
            "unexpected error: {err}"
        );

        assert_eq!(
            vs.val_status.read(&validator).unwrap(),
            outbe_validatorset::logic::status::ACTIVE
        );
        assert!(!vs.pending_set_change.read().unwrap());
        assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 1);
        assert_eq!(staking.stake_amount.read(&validator).unwrap(), stake);
        assert_eq!(staking.total_staked.read().unwrap(), stake);
    });
}

#[test]
fn slash_window_never_force_exits_a_protected_validator() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.config_allow_protected.write(true).unwrap();

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, U256::in_units(100u64));

        // Mark as protected
        oracle.protected_validator.write(&v1, true).unwrap();

        // Simulate many misses
        for _ in 0..20 {
            oracle.increment_miss(&v1).unwrap();
        }

        crate::tally::slash_and_reset_counters(&mut oracle, 10000).unwrap();

        // Counters reset but validator NOT force-exited
        assert_eq!(oracle.penalty_miss_count.read(&v1).unwrap(), 0);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let info = vs.get_validator(v1).unwrap().unwrap();
        assert_eq!(info.status, outbe_validatorset::logic::status::ACTIVE);
    });
}
#[test]
fn begin_block_scurve_hook_records_the_daily_peak() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let pair_id = oracle.register_pair(COEN, usd()).unwrap();
        let pair = pair_key(COEN, usd());

        let day_1 = crate::scurve::DAY_SECONDS;
        let day_2 = 2 * crate::scurve::DAY_SECONDS;
        let day_3 = 3 * crate::scurve::DAY_SECONDS;
        let day_4 = 4 * crate::scurve::DAY_SECONDS;
        // Three fully-closed days forming a peak at day_2: 100 < 150 > 120.
        oracle
            .write_snapshot(day_1 + 60, &[(pair, U256::in_units(100u64), SCALE_1E18)])
            .unwrap();
        oracle
            .write_snapshot(day_2 + 60, &[(pair, U256::in_units(150u64), SCALE_1E18)])
            .unwrap();
        oracle
            .write_snapshot(day_3 + 60, &[(pair, U256::in_units(120u64), SCALE_1E18)])
            .unwrap();

        // Hook fires on the first block of day_4 — the current day has NO
        // close yet, mirroring the real start-of-day boundary block.
        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(4, day_4 + 120, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();

        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert_eq!(oracle.scurve_pair_id.read(&0).unwrap(), pair_id);
        assert_eq!(oracle.scurve_peak_day.read(&0).unwrap(), day_2);
        assert_eq!(
            oracle.scurve_peak_price.read(&0).unwrap(),
            U256::in_units(150u64)
        );
        assert_eq!(oracle.scurve_last_processed_day.read().unwrap(), day_4);

        let active_value =
            crate::scurve::get_max_active_scurve_value(&oracle, pair_id, day_4).unwrap();
        assert!(!active_value.is_zero());
        assert!(active_value < U256::in_units(150u64));
    });
}
#[test]
fn begin_block_finalizes_the_closed_utc_day() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.config_is_initialized.write(true).unwrap();
        oracle.config_vote_period.write(2).unwrap();
        let coen_id = oracle.register_pair(COEN, usd()).unwrap();
        let coen = pair_key(COEN, usd());

        let day_d = 20260624u32;
        let day_d1 = 20260625u32;
        let day_d2 = 20260626u32;
        let d_start = outbe_primitives::time::date_key_to_utc_timestamp(day_d);
        let d1_start = outbe_primitives::time::date_key_to_utc_timestamp(day_d1);
        let d2_start = outbe_primitives::time::date_key_to_utc_timestamp(day_d2);

        oracle
            .write_snapshot(
                d_start + 1_000,
                &[(coen, U256::from(170u64), U256::from(1u64))],
            )
            .unwrap();

        // First block of day D+1 → day D is now fully closed and finalized.
        // Odd block number avoids the vote-period tally path (period == 2).
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(11, d1_start + 5, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();

        assert_eq!(oracle.utc_day_vwap_last_finalized.read().unwrap(), day_d);
        assert_eq!(
            oracle.get_utc_day_vwap_for_pair_id(day_d, coen_id).unwrap(),
            Some(U256::from(170u64))
        );
        // The in-progress current day is not finalized.
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair_id(day_d1, coen_id)
                .unwrap(),
            None
        );

        // Idempotent: a later block on the same UTC day neither advances the
        // watermark nor re-finalizes.
        let ctx2 = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(13, d1_start + 50, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&ctx2).unwrap();
        assert_eq!(oracle.utc_day_vwap_last_finalized.read().unwrap(), day_d);

        // Next rollover finalizes the next day contiguously (non-zero
        // watermark path).
        oracle
            .write_snapshot(
                d1_start + 2_000,
                &[(coen, U256::from(190u64), U256::from(1u64))],
            )
            .unwrap();
        let ctx3 = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(15, d2_start + 5, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&ctx3).unwrap();
        assert_eq!(oracle.utc_day_vwap_last_finalized.read().unwrap(), day_d1);
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair_id(day_d1, coen_id)
                .unwrap(),
            Some(U256::from(190u64))
        );
    });
}
