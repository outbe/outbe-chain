use super::*;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_validatorset::test_support::StorageOverrides;

#[test]
fn run_tally_counts_an_abstain_for_every_silent_validator() {
    with_coen_usdt_voter(|_storage, oracle, v1| {
        // No votes submitted -> all abstain
        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_penalty_counters(
            oracle,
            &[(v1, Penalty::Abstain, 1), (v1, Penalty::Success, 0)],
        );
    });
}

#[test]
fn begin_block_tallies_only_on_a_vote_period_boundary() {
    with_coen_usdt_voter(|storage, oracle, v1| {
        oracle
            .submit_vote(v1, &[(COEN, USDT, fixed18(42), SCALE_1E18)])
            .unwrap();

        // 2020-01-02 keeps the 12-second gap. A block on 2020-01-01 makes the
        // zero watermark step 2019-12-31, and begin_block refuses that step.
        let day = outbe_primitives::time::date_key_to_utc_timestamp(20_200_102);
        // Block 1: not a vote period boundary (period=2), no tally
        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, day + 12, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
        assert!(oracle.vote_exists.read(&v1).unwrap()); // vote still exists

        // Block 2: vote period boundary, tally runs
        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, day + 24, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
        assert!(!oracle.vote_exists.read(&v1).unwrap()); // votes cleared

        let rate = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, fixed18(42));
    });
}

#[test]
fn slash_window_resets_penalty_counters_at_the_window_end() {
    with_oracle(|storage, oracle| {
        let v1 = register_voter(&storage);

        // Simulate many misses (below 5% success rate)
        for _ in 0..20 {
            oracle.increment_miss(&v1).unwrap();
        }
        oracle.increment_success(&v1).unwrap(); // 1 success out of 21 = 4.76% < 5%

        // Run slash and reset
        crate::tally::slash_and_reset_counters(oracle, 10000).unwrap();

        // Counters should be reset
        assert_penalty_counters(oracle, &[(v1, Penalty::Success, 0), (v1, Penalty::Miss, 0)]);

        // Validator should be force-exited (check via ValidatorSet)
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        assert!(matches!(
            vs.validator_lifecycle(v1).unwrap(),
            ValidatorLifecycle::JailRetained(_) | ValidatorLifecycle::Jail(_)
        ));
    });
}

#[test]
fn slash_window_timestamp_overflow_preserves_all_validator_counters_for_retry() {
    with_oracle(|storage, oracle| {
        let healthy = Address::new([0x33; 20]);
        let penalized = Address::new([0x44; 20]);
        let stake = native_coen(100);
        register_validator(storage.clone(), healthy, stake);
        register_validator(storage.clone(), penalized, stake);
        oracle.increment_success(&healthy).unwrap();
        oracle.increment_miss(&penalized).unwrap();
        oracle
            .config_slash_fraction
            .write(SCALE_1E18 / U256::from(10))
            .unwrap();
        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&penalized, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        storage
            .set_balance(outbe_primitives::addresses::STAKING_ADDRESS, stake)
            .unwrap();
        for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
            storage.set_block_timestamp(timestamp).unwrap();
            assert!(matches!(
                crate::tally::slash_and_reset_counters(oracle, 10_000),
                Err(outbe_primitives::error::PrecompileError::Revert(message)) if message == "timestamp exceeds u64"
            ));
            assert_eq!(oracle.penalty_success_count.read(&healthy).unwrap(), 1);
            assert_eq!(oracle.penalty_miss_count.read(&penalized).unwrap(), 1);
            assert!(
                outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                    .validator_lifecycle(penalized)
                    .unwrap()
                    .is_active_status()
            );
            assert_eq!(staking.get_stake(penalized).unwrap(), stake);
            assert_eq!(
                storage
                    .balance(outbe_primitives::addresses::STAKING_ADDRESS)
                    .unwrap(),
                stake
            );
        }
        storage.set_block_timestamp(U256::from(10_000)).unwrap();
        crate::tally::slash_and_reset_counters(oracle, 10_000).unwrap();
        assert_eq!(oracle.penalty_success_count.read(&healthy).unwrap(), 0);
        assert_eq!(oracle.penalty_miss_count.read(&penalized).unwrap(), 0);
        assert!(matches!(
            outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                .validator_lifecycle(penalized)
                .unwrap(),
            ValidatorLifecycle::JailRetained(_) | ValidatorLifecycle::Jail(_)
        ));
        assert_eq!(
            staking.get_stake(penalized).unwrap(),
            stake * U256::from(9) / U256::from(10)
        );
    });
}

#[test]
fn slash_window_rejects_unbounded_validator_work() {
    with_oracle(|storage, oracle| {
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

        let err = crate::tally::slash_and_reset_counters(oracle, 10_000).unwrap_err();
        assert_typed_revert(
            &err,
            crate::errors::OracleError::SlashWindowValidatorSetExceedsCap {
                actual: crate::tally::MAX_ORACLE_SLASH_WINDOW_VALIDATORS + 1,
                cap: crate::tally::MAX_ORACLE_SLASH_WINDOW_VALIDATORS,
            },
        );
    });
}

/// The setter admits up to `CONSENSUS_VALIDATOR_BOUND` validators, so the slash
/// window must process a registry above the old fixed cap of 128.
#[test]
fn slash_window_processes_every_admissible_registry() {
    with_oracle(|storage, oracle| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_is_initialized.write(true).unwrap();
        vs.config_owner.write(Address::ZERO).unwrap();
        vs.config_epoch_length_blocks.write(3600).unwrap();
        vs.set_config_max_validators(outbe_validatorset::runtime::CONSENSUS_VALIDATOR_BOUND)
            .unwrap();

        for i in 1..=129u32 {
            let mut bytes = [0u8; 20];
            bytes[16..].copy_from_slice(&i.to_be_bytes());
            register_validator(storage.clone(), Address::new(bytes), U256::from(1u64));
        }

        crate::tally::slash_and_reset_counters(oracle, 10_000).unwrap();
    });
}

/// Genesis rejects such a slash_fraction. If one reaches storage anyway, the
/// slash window must fail with an error, not panic and stop every node.
#[test]
fn slash_window_errors_instead_of_panicking_on_an_oversized_slash_fraction() {
    with_oracle(|storage, oracle| {
        let v1 = register_voter(&storage);
        for _ in 0..20 {
            oracle.increment_miss(&v1).unwrap();
        }
        oracle.config_slash_fraction.write(U256::MAX).unwrap();

        let err = crate::tally::slash_and_reset_counters(oracle, 10_000).unwrap_err();
        assert_typed_revert(
            &err,
            crate::errors::OracleError::SlashPercentOutOfRange {
                slash_fraction: U256::MAX,
            },
        );
    });
}

#[test]
fn slash_window_rolls_back_slash_state_when_force_exit_fails() {
    with_oracle(|storage, oracle| {
        oracle
            .config_slash_fraction
            .write(SCALE_1E18 / U256::from(10u64))
            .unwrap();

        let validator = Address::new([0x33; 20]);
        let stake = native_coen(100);
        register_waiting_for_readiness(storage.clone(), validator, stake);
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.test_set_pending_set_change(false).unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        oracle
            .storage
            .set_balance(outbe_primitives::addresses::STAKING_ADDRESS, stake)
            .unwrap();

        oracle.increment_miss(&validator).unwrap();

        let err = crate::tally::slash_and_reset_counters(oracle, 10_000).unwrap_err();
        assert_typed_revert(
            &err,
            outbe_validatorset::errors::ValidatorSetError::CannotPunishValidatorStatus {
                action: "jail",
                status: outbe_validatorset::runtime::status::PENDING,
            },
        );

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
        assert_eq!(vs.validator_state(validator).unwrap().bonded_stake(), stake);
        assert!(matches!(
            vs.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));

        assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 1);
    });
}

#[test]
fn slash_window_rolls_back_the_forced_exit_when_slashing_fails() {
    with_oracle(|storage, oracle| {
        oracle
            .config_slash_fraction
            .write(SCALE_1E18 / U256::from(10u64))
            .unwrap();

        let validator = Address::new([0x44; 20]);
        let stake = native_coen(100);
        register_validator(storage.clone(), validator, stake);
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.test_set_pending_set_change(false).unwrap();

        let staking = outbe_staking::contract::Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        oracle
            .storage
            .set_balance(outbe_primitives::addresses::STAKING_ADDRESS, U256::ZERO)
            .unwrap();

        oracle.increment_miss(&validator).unwrap();

        let err = crate::tally::slash_and_reset_counters(oracle, 10_000).unwrap_err();
        assert!(
            err.to_string().contains("insufficient") || err.to_string().contains("balance"),
            "unexpected error: {err}"
        );

        assert!(vs
            .validator_lifecycle(validator)
            .unwrap()
            .is_active_status());
        assert!(!vs.has_pending_set_change().unwrap());
        assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 1);
        assert_eq!(staking.stake_amount.read(&validator).unwrap(), stake);
        assert_eq!(staking.total_staked.read().unwrap(), stake);
    });
}

#[test]
fn begin_block_scurve_hook_records_the_daily_peak() {
    with_oracle(|storage, oracle| {
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();

        // Shift the epoch day grid onto 2020-01-01. Offsets inside each day stay.
        let origin = outbe_primitives::time::date_key_to_utc_timestamp(20_200_101);
        let day_1 = origin + SECONDS_PER_DAY;
        let day_2 = origin + 2 * SECONDS_PER_DAY;
        let day_3 = origin + 3 * SECONDS_PER_DAY;
        let day_4 = origin + 4 * SECONDS_PER_DAY;
        // Three fully-closed days forming a peak at day_2: 100 < 150 > 120.
        oracle
            .write_snapshot(
                day_1 + 60,
                &[(pair_key(COEN, usd()), coen_iso(100), coen_iso(1))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                day_2 + 60,
                &[(pair_key(COEN, usd()), coen_iso(150), coen_iso(1))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                day_3 + 60,
                &[(pair_key(COEN, usd()), coen_iso(120), coen_iso(1))],
            )
            .unwrap();

        // The hook fires on the first block of day_4. The current day has NO
        // close yet. This mirrors the real start-of-day boundary block.
        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(4, day_4 + 120, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();

        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert_eq!(
            oracle.scurve_pair.read_pair(&0u32).unwrap(),
            pair_key(COEN, usd())
        );
        assert_eq!(oracle.scurve_peak_day.read(&0).unwrap(), day_2);
        assert_eq!(oracle.scurve_peak_price.read(&0).unwrap(), coen_iso(150));
        assert_eq!(oracle.scurve_last_processed_day.read().unwrap(), day_4);

        let active_value =
            crate::scurve::get_max_active_scurve_value(oracle, pair_key(COEN, usd()), day_4)
                .unwrap();
        assert!(!active_value.is_zero());
        assert!(active_value < coen_iso(150));

        // The begin-block owner must keep the same chain alive after the first
        // 128-day coefficient period. No expiry/eviction or successor row is
        // required for continuation.
        let day_130 = day_2 + 128 * SECONDS_PER_DAY;
        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(130, day_130 + 120, 1),
            storage,
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert_eq!(
            crate::scurve::get_max_active_scurve_value(oracle, pair_key(COEN, usd()), day_130)
                .unwrap(),
            crate::scurve::compute_scurve_value(coen_iso(150), 128)
        );
    });
}

#[test]
fn begin_block_scurve_hook_processes_only_registered_reference_pairs() {
    with_oracle(|storage, oracle| {
        let usd_pair = AddressPair::new_coen_to(840);
        let eur_pair = AddressPair::new_coen_to(978);
        let cad_pair = AddressPair::new_coen_to(124); // active, priced, non-reference
        let generic_pair = AddressPair::from_addresses(COEN, USDT);
        oracle.register_pair(usd_pair).unwrap();
        oracle.register_pair(eur_pair).unwrap();
        oracle.register_pair(cad_pair).unwrap();
        oracle.register_pair(generic_pair).unwrap();

        // Shift the epoch day grid onto 2020-01-01. Offsets inside each day stay.
        let origin = outbe_primitives::time::date_key_to_utc_timestamp(20_200_101);
        let day_1 = origin + SECONDS_PER_DAY;
        let day_2 = origin + 2 * SECONDS_PER_DAY;
        let day_3 = origin + 3 * SECONDS_PER_DAY;
        let day_4 = origin + 4 * SECONDS_PER_DAY;
        for (timestamp, usd_price, eur_price, cad_price, generic_price) in [
            (
                day_1 + 60,
                coen_iso(100),
                coen_iso(90),
                coen_iso(80),
                fixed18(2),
            ),
            (
                day_2 + 60,
                coen_iso(150),
                coen_iso(140),
                coen_iso(130),
                fixed18(3),
            ),
            (
                day_3 + 60,
                coen_iso(120),
                coen_iso(110),
                coen_iso(100),
                fixed18(2),
            ),
        ] {
            oracle
                .write_snapshot(
                    timestamp,
                    &[
                        (usd_pair, usd_price, coen_iso(1)),
                        (eur_pair, eur_price, coen_iso(1)),
                        (cad_pair, cad_price, coen_iso(1)),
                        (generic_pair, generic_price, SCALE_1E18),
                    ],
                )
                .unwrap();
        }

        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(4, day_4 + 120, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();

        assert_eq!(oracle.scurve_count.read().unwrap(), 2);
        assert_eq!(oracle.scurve_pair.read_pair(&0).unwrap(), usd_pair);
        assert_eq!(oracle.scurve_pair.read_pair(&1).unwrap(), eur_pair);
        assert_eq!(oracle.scurve_peak_price.read(&0).unwrap(), coen_iso(150));
        assert_eq!(oracle.scurve_peak_price.read(&1).unwrap(), coen_iso(140));
        assert_ne!(oracle.scurve_pair.read_pair(&0).unwrap(), cad_pair);
        assert_ne!(oracle.scurve_pair.read_pair(&1).unwrap(), cad_pair);
        assert_eq!(oracle.scurve_last_processed_day.read().unwrap(), day_4);
    });
}

#[test]
fn begin_block_finalizes_the_closed_utc_day() {
    with_bare_oracle(|storage, oracle| {
        oracle.config_is_initialized.write(true).unwrap();
        oracle.config_vote_period.write(2).unwrap();
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
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
                &[(pair_key(COEN, usd()), coen_iso(170), coen_iso(1))],
            )
            .unwrap();

        // First block of day D+1 -> day D is now fully closed and finalized.
        // Odd block number avoids the vote-period tally path (period == 2).
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(11, d1_start + 5, 1),
            storage.clone(),
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();

        assert_eq!(oracle.utc_day_vwap_last_finalized.read().unwrap(), day_d);
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(day_d, oracle.pair_index_of(coen).unwrap())
                .unwrap(),
            Some(coen_iso(170))
        );
        // The in-progress current day is not finalized.
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(day_d1, oracle.pair_index_of(coen).unwrap())
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
                &[(pair_key(COEN, usd()), coen_iso(190), coen_iso(1))],
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
                .get_utc_day_vwap_for_pair(day_d1, oracle.pair_index_of(coen).unwrap())
                .unwrap(),
            Some(coen_iso(190))
        );
    });
}
