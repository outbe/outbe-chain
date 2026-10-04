use super::*;

#[test]
fn run_tally_counts_an_abstain_for_every_silent_validator() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, native_coen(100));

        // No votes submitted -> all abstain
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
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, native_coen(100));
        oracle
            .submit_vote(v1, &[(COEN, USDT, fixed18(42), SCALE_1E18)])
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

        let rate = oracle.get_exchange_rate(COEN, USDT).unwrap();
        assert_eq!(rate, fixed18(42));
    });
}

#[test]
fn slash_window_resets_penalty_counters_at_the_window_end() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, native_coen(100));

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
        assert!(matches!(
            vs.validator_lifecycle(v1).unwrap(),
            ValidatorLifecycle::JailRetained(_) | ValidatorLifecycle::Jail(_)
        ));
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
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
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

        let err = crate::tally::slash_and_reset_counters(&mut oracle, 10_000).unwrap_err();
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
fn slash_window_never_force_exits_a_protected_validator() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.config_allow_protected.write(true).unwrap();

        let v1 = Address::new([0x11; 20]);
        register_validator(storage.clone(), v1, native_coen(100));

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
        assert!(vs.validator_lifecycle(v1).unwrap().is_active_status());
    });
}
#[test]
fn begin_block_scurve_hook_records_the_daily_peak() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.reference_currencies.push(840).unwrap();
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();

        let day_1 = crate::scurve::DAY_SECONDS;
        let day_2 = 2 * crate::scurve::DAY_SECONDS;
        let day_3 = 3 * crate::scurve::DAY_SECONDS;
        let day_4 = 4 * crate::scurve::DAY_SECONDS;
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

        // Hook fires on the first block of day_4 - the current day has NO
        // close yet, mirroring the real start-of-day boundary block.
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
            crate::scurve::get_max_active_scurve_value(&oracle, pair_key(COEN, usd()), day_4)
                .unwrap();
        assert!(!active_value.is_zero());
        assert!(active_value < coen_iso(150));

        // The begin-block owner must keep the same chain alive after the first
        // 128-day coefficient period; no expiry/eviction or successor row is
        // required for continuation.
        let day_130 = day_2 + 128 * crate::scurve::DAY_SECONDS;
        let runtime_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(130, day_130 + 120, 1),
            storage,
        );
        <crate::lifecycle::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
        assert_eq!(oracle.scurve_count.read().unwrap(), 1);
        assert_eq!(
            crate::scurve::get_max_active_scurve_value(&oracle, pair_key(COEN, usd()), day_130)
                .unwrap(),
            crate::scurve::compute_scurve_value(coen_iso(150), 128)
        );
    });
}

#[test]
fn begin_block_scurve_hook_processes_only_registered_reference_pairs() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.reference_currencies.push(840).unwrap();
        oracle.reference_currencies.push(978).unwrap();
        oracle.reference_currencies.push(392).unwrap(); // no pair: no-op

        let usd_pair = AddressPair::new_coen_to(840);
        let eur_pair = AddressPair::new_coen_to(978);
        let cad_pair = AddressPair::new_coen_to(124); // active, priced, non-reference
        let generic_pair = AddressPair::from_addresses(COEN, USDT);
        oracle.register_pair(usd_pair).unwrap();
        oracle.register_pair(eur_pair).unwrap();
        oracle.register_pair(cad_pair).unwrap();
        oracle.register_pair(generic_pair).unwrap();

        let day_1 = crate::scurve::DAY_SECONDS;
        let day_2 = 2 * crate::scurve::DAY_SECONDS;
        let day_3 = 3 * crate::scurve::DAY_SECONDS;
        let day_4 = 4 * crate::scurve::DAY_SECONDS;
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
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
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
