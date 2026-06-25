#[cfg(test)]
mod oracle_tests {
    use alloy_primitives::{Address, U256};
    use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::units::Units;

    use crate::contract::{OracleContract, SCALE_1E18};

    /// Each test gets a fresh `HashMapStorageProvider` via this helper.
    /// No shared mutable state between tests; contracts read/write through the
    /// scoped `StorageHandle` passed into the closure.
    fn with_storage<F: FnOnce(StorageHandle)>(f: F) {
        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, f);
    }

    fn with_storage_at<F: FnOnce(StorageHandle)>(timestamp: u64, f: F) {
        let mut storage = HashMapStorageProvider::new(1);
        storage.set_timestamp(U256::from(timestamp));
        StorageHandle::enter(&mut storage, f);
    }

    #[test]
    fn test_pair_registration() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());

            // Register first pair
            let id1 = oracle.register_pair("COEN", "USDT").unwrap();
            assert_eq!(id1, 1);

            // Register second pair
            let id2 = oracle.register_pair("ETH", "USDT").unwrap();
            assert_eq!(id2, 2);

            // Verify lookup
            assert_eq!(oracle.get_pair_id("COEN", "USDT").unwrap(), 1);
            assert_eq!(oracle.get_pair_id("ETH", "USDT").unwrap(), 2);
            assert_eq!(oracle.get_pair_id("BTC", "USDT").unwrap(), 0); // not registered

            // Verify vote targets
            assert!(oracle.is_vote_target("COEN", "USDT").unwrap());
            assert!(oracle.is_vote_target("ETH", "USDT").unwrap());
            assert!(!oracle.is_vote_target("BTC", "USDT").unwrap());

            // Duplicate registration fails
            assert!(oracle.register_pair("COEN", "USDT").is_err());

            // Pair count
            assert_eq!(oracle.pair_count.read().unwrap(), 2);
            assert_eq!(oracle.pair_id_to_base.read_string(&1).unwrap(), "COEN");
            assert_eq!(oracle.pair_id_to_quote.read_string(&1).unwrap(), "USDT");
            assert_eq!(oracle.pair_id_to_base.read_string(&2).unwrap(), "ETH");
            assert_eq!(oracle.pair_id_to_quote.read_string(&2).unwrap(), "USDT");
        });
    }

    #[test]
    fn test_get_pairs_returns_ids_symbols_and_active() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());

            oracle.register_pair("COEN", "USDT").unwrap();
            oracle.register_pair("ETH", "USDC").unwrap();
            oracle.register_pair("BTC", "USDC").unwrap();

            // Deactivate the middle pair to exercise the isActive flag.
            oracle
                .deactivate_vote_target(Address::ZERO, "ETH", "USDC")
                .unwrap();

            let (ids, bases, quotes, active) = oracle.get_pairs().unwrap();

            // Parallel arrays are aligned and 1-indexed in registration order.
            assert_eq!(ids, vec![1, 2, 3]);
            assert_eq!(bases, vec!["COEN", "ETH", "BTC"]);
            assert_eq!(quotes, vec!["USDT", "USDC", "USDC"]);
            assert_eq!(active, vec![true, false, true]);
        });
    }

    #[test]
    fn test_get_pairs_empty() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            let (ids, bases, quotes, active) = oracle.get_pairs().unwrap();
            assert!(ids.is_empty());
            assert!(bases.is_empty());
            assert!(quotes.is_empty());
            assert!(active.is_empty());
        });
    }

    #[test]
    fn test_pair_hash_determinism() {
        let h1 = OracleContract::pair_hash("COEN", "USDT");
        let h2 = OracleContract::pair_hash("COEN", "USDT");
        assert_eq!(h1, h2);

        let h3 = OracleContract::pair_hash("ETH", "USDT");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_exchange_rate_read_write() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();

            // Set rate (system call)
            let rate = U256::from(1_500_000_000_000_000_000u128); // 1.5
            oracle
                .set_exchange_rate(Address::ZERO, "COEN", "USDT", rate, 100, 1200)
                .unwrap();

            // Read back
            let (r, block, ts) = oracle.get_exchange_rate("COEN", "USDT").unwrap();
            assert_eq!(r, rate);
            assert_eq!(block, 100);
            assert_eq!(ts, 1200);
        });
    }

    #[test]
    fn test_exchange_rate_non_system_rejected() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();

            let caller = Address::new([1u8; 20]);
            let result = oracle.set_exchange_rate(caller, "COEN", "USDT", U256::from(1u64), 0, 0);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_exchange_rate_unregistered_pair() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            assert!(oracle.get_exchange_rate("BTC", "USDT").is_err());
        });
    }

    #[test]
    fn test_config_read_write() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());

            oracle.config_vote_period.write(2).unwrap();
            oracle
                .config_reward_band
                .write(U256::from(20_000_000_000_000_000u128))
                .unwrap();
            oracle.config_slash_window.write(96).unwrap();
            oracle.config_lookback_duration.write(86400).unwrap();
            oracle.config_enabled.write(true).unwrap();
            oracle.config_is_initialized.write(true).unwrap();

            assert_eq!(oracle.config_vote_period.read().unwrap(), 2);
            assert_eq!(
                oracle.config_reward_band.read().unwrap(),
                U256::from(20_000_000_000_000_000u128)
            );
            assert_eq!(oracle.config_slash_window.read().unwrap(), 96);
            assert_eq!(oracle.config_lookback_duration.read().unwrap(), 86400);
            assert!(oracle.config_enabled.read().unwrap());
            assert!(oracle.config_is_initialized.read().unwrap());
        });
    }

    #[test]
    fn test_penalty_counters() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let validator = Address::new([0x11; 20]);

            oracle.increment_success(&validator).unwrap();
            oracle.increment_success(&validator).unwrap();
            oracle.increment_miss(&validator).unwrap();
            oracle.increment_abstain(&validator).unwrap();

            assert_eq!(oracle.penalty_success_count.read(&validator).unwrap(), 2);
            assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 1);
            assert_eq!(oracle.penalty_abstain_count.read(&validator).unwrap(), 1);

            oracle.reset_penalty_counter(&validator).unwrap();
            assert_eq!(oracle.penalty_success_count.read(&validator).unwrap(), 0);
            assert_eq!(oracle.penalty_miss_count.read(&validator).unwrap(), 0);
            assert_eq!(oracle.penalty_abstain_count.read(&validator).unwrap(), 0);
        });
    }

    #[test]
    fn test_snapshot_write_and_read() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();

            // Write 3 snapshots
            let entries = vec![(1u32, U256::in_units(100u64), U256::in_units(1000u64))];
            oracle.write_snapshot(1000, &entries).unwrap();

            let entries2 = vec![(1u32, U256::in_units(200u64), U256::in_units(2000u64))];
            oracle.write_snapshot(2000, &entries2).unwrap();

            let entries3 = vec![(1u32, U256::in_units(300u64), U256::in_units(3000u64))];
            oracle.write_snapshot(3000, &entries3).unwrap();

            assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 3);
            assert_eq!(oracle.snapshot_oldest_idx.read().unwrap(), 0);

            // Calculate VWAP over all snapshots
            // VWAP = (100*1000 + 200*2000 + 300*3000) / (1000 + 2000 + 3000)
            //      = (100000 + 400000 + 900000) / 6000
            //      = 1400000 / 6000
            //      = 233.333...
            let vwap = oracle.calculate_vwap(1, 0, 5000).unwrap();
            // TODO is it correct??
            let expected = U256::in_units(1_400_000u64) * SCALE_1E18 / (U256::in_units(6_000u64));
            assert_eq!(vwap, expected);
        });
    }

    #[test]
    fn test_snapshot_time_filtering() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();

            let entries1 = vec![(1u32, U256::in_units(100u64), SCALE_1E18)];
            oracle.write_snapshot(1000, &entries1).unwrap();

            let entries2 = vec![(1u32, U256::in_units(200u64), SCALE_1E18)];
            oracle.write_snapshot(2000, &entries2).unwrap();

            let entries3 = vec![(1u32, U256::in_units(300u64), SCALE_1E18)];
            oracle.write_snapshot(3000, &entries3).unwrap();

            // VWAP from 1500..2500 should only include snapshot at 2000
            let vwap = oracle.calculate_vwap(1, 1500, 2500).unwrap();
            assert_eq!(vwap, U256::in_units(200u64));

            // VWAP from 2500..3500 should only include snapshot at 3000
            let vwap = oracle.calculate_vwap(1, 2500, 3500).unwrap();
            assert_eq!(vwap, U256::in_units(300u64));
        });
    }

    #[test]
    fn test_vwap_empty_window() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            // No snapshots at all
            assert!(oracle.calculate_vwap(1, 0, 1000).is_err());
        });
    }

    #[test]
    fn test_vwap_zero_volume_fallback() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();

            // Zero-volume entries → equal-weight averaging
            let entries1 = vec![(1u32, U256::in_units(100u64), U256::ZERO)];
            oracle.write_snapshot(1000, &entries1).unwrap();

            let entries2 = vec![(1u32, U256::in_units(200u64), U256::ZERO)];
            oracle.write_snapshot(2000, &entries2).unwrap();

            // Equal-weight: (100 + 200) / 2 = 150
            let vwap = oracle.calculate_vwap(1, 0, 3000).unwrap();
            // With zero volumes, each gets SCALE_1E18 weight:
            // sum(rate * 1e18) / sum(1e18) = (100*1e18 + 200*1e18) / (2*1e18) = 150
            let expected = (U256::in_units(100u64) * SCALE_1E18
                + U256::in_units(200u64) * SCALE_1E18)
                / (U256::in_units(2u64));
            assert_eq!(vwap, expected);
        });
    }

    #[test]
    fn test_multiple_pairs_in_snapshot() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let id1 = oracle.register_pair("COEN", "USDT").unwrap();
            let id2 = oracle.register_pair("ETH", "USDT").unwrap();

            let entries = vec![
                (id1, U256::in_units(1u64), U256::in_units(100u64)),
                (id2, U256::in_units(2000u64), U256::in_units(50u64)),
            ];
            oracle.write_snapshot(1000, &entries).unwrap();

            // VWAP for COEN should be 1
            let vwap_coen = oracle.calculate_vwap(id1, 0, 2000).unwrap();
            assert_eq!(vwap_coen, SCALE_1E18);

            // VWAP for ETH should be 2000
            let vwap_eth = oracle.calculate_vwap(id2, 0, 2000).unwrap();
            assert_eq!(vwap_eth, U256::in_units(2000u64));
        });
    }

    // -----------------------------------------------------------------------
    // Phase 2: Tally integration tests
    // -----------------------------------------------------------------------

    /// Helper: initialize oracle config and register a pair.
    fn init_oracle(oracle: &mut OracleContract) {
        oracle.config_vote_period.write(2).unwrap();
        oracle
            .config_reward_band
            .write(U256::from(20_000_000_000_000_000u128))
            .unwrap(); // 0.02
        oracle.config_slash_window.write(96).unwrap();
        oracle
            .config_min_valid_per_window
            .write(U256::from(50_000_000_000_000_000u128))
            .unwrap(); // 0.05
        oracle.config_slash_fraction.write(U256::ZERO).unwrap();
        oracle.config_lookback_duration.write(86400).unwrap();
        oracle.config_enabled.write(true).unwrap();
        oracle.config_is_initialized.write(true).unwrap();
    }

    /// Helper: register a validator in the ValidatorSet with given stake.
    /// Uses the first byte of addr as the pubkey seed to avoid BLS pubkey collision.
    fn register_validator(storage: StorageHandle, addr: Address, stake: U256) {
        use outbe_validatorset::logic::status;

        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        // Only write config once (if not already initialized)
        if !vs.config_is_initialized.read().unwrap() {
            vs.config_is_initialized.write(true).unwrap();
            vs.config_max_validators.write(128).unwrap();
            vs.config_min_stake.write(U256::in_units(1u64)).unwrap();
            vs.config_epoch_length_blocks.write(3600).unwrap();
            vs.config_owner.write(Address::ZERO).unwrap();
        }

        // Generate unique pubkey from address
        let mut pubkey = [0u8; 48];
        pubkey[..20].copy_from_slice(addr.as_slice());
        vs.register_validator(Address::ZERO, addr, &pubkey).unwrap();
        // Set stake and status to ACTIVE
        vs.val_stake.write(&addr, stake).unwrap();
        vs.val_status.write(&addr, status::ACTIVE).unwrap();
    }

    #[test]
    fn test_vote_submission_and_clear() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let validator = Address::new([0x11; 20]);
            register_validator(storage.clone(), validator, U256::in_units(100u64));

            let pair_hash = OracleContract::pair_hash("COEN", "USDT");
            let rate = U256::in_units(50u64);
            let volume = U256::in_units(1000u64);

            // Submit vote
            oracle
                .submit_vote(validator, &[(pair_hash, rate, volume)])
                .unwrap();

            // Verify vote stored
            assert!(oracle.vote_exists.read(&validator).unwrap());
            assert_eq!(oracle.vote_tuple_count.read(&validator).unwrap(), 1);
            assert_eq!(oracle.voter_list.len().unwrap(), 1);

            // Double vote should fail
            assert!(oracle
                .submit_vote(validator, &[(pair_hash, rate, volume)])
                .is_err());

            // Clear
            oracle.clear_votes().unwrap();
            assert!(!oracle.vote_exists.read(&validator).unwrap());
            assert_eq!(oracle.voter_list.len().unwrap(), 0);
        });
    }

    #[test]
    fn test_run_tally_single_validator() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let validator = Address::new([0x11; 20]);
            register_validator(storage.clone(), validator, U256::in_units(100u64));

            let pair_hash = OracleContract::pair_hash("COEN", "USDT");
            let rate = U256::in_units(50u64);
            let volume = U256::in_units(1000u64);

            oracle
                .submit_vote(validator, &[(pair_hash, rate, volume)])
                .unwrap();

            // Run tally
            crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

            // Exchange rate should be updated to the voted rate
            let (stored_rate, block, ts) = oracle.get_exchange_rate("COEN", "USDT").unwrap();
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
    fn test_run_tally_convergent_votes() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let v1 = Address::new([0x11; 20]);
            let v2 = Address::new([0x22; 20]);
            let v3 = Address::new([0x33; 20]);

            register_validator(storage.clone(), v1, U256::in_units(100u64));
            register_validator(storage.clone(), v2, U256::in_units(200u64));
            register_validator(storage.clone(), v3, U256::in_units(100u64));

            let pair_hash = OracleContract::pair_hash("COEN", "USDT");

            // All vote very close: 1000, 1001, 1002 (spread < 0.2% of median)
            // With 2% reward band, all should be within band.
            let base = U256::in_units(1000u64);
            oracle
                .submit_vote(v1, &[(pair_hash, base, SCALE_1E18)])
                .unwrap();
            oracle
                .submit_vote(v2, &[(pair_hash, base + SCALE_1E18, SCALE_1E18)])
                .unwrap();
            oracle
                .submit_vote(v3, &[(pair_hash, base + U256::in_units(2u64), SCALE_1E18)])
                .unwrap();

            crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

            // Weighted median: powers 100, 200, 100. Total=400, half=200.
            // Sorted: 1000(100), 1001(200), 1002(100).
            // Cumsum: 100(<200), 300(>=200) → median = 1001.
            let (rate, _, _) = oracle.get_exchange_rate("COEN", "USDT").unwrap();
            assert_eq!(rate, U256::in_units(1001u64));

            // Reward spread = max(std_dev, 1001 * 0.02 / 2) = max(~0.816, ~10.01) = ~10.01
            // All votes within [990.99, 1011.01] → all win
            assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
            assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
            assert_eq!(oracle.penalty_success_count.read(&v3).unwrap(), 1);
        });
    }

    #[test]
    fn test_run_tally_with_outlier() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let v1 = Address::new([0x11; 20]);
            let v2 = Address::new([0x22; 20]);
            let v3 = Address::new([0x33; 20]);

            register_validator(storage.clone(), v1, U256::in_units(100u64));
            register_validator(storage.clone(), v2, U256::in_units(200u64));
            register_validator(storage.clone(), v3, U256::in_units(100u64));

            let pair_hash = OracleContract::pair_hash("COEN", "USDT");

            // v1 and v2 vote 50, v3 votes 500 (extreme outlier)
            oracle
                .submit_vote(v1, &[(pair_hash, U256::in_units(50u64), SCALE_1E18)])
                .unwrap();
            oracle
                .submit_vote(v2, &[(pair_hash, U256::in_units(50u64), SCALE_1E18)])
                .unwrap();
            oracle
                .submit_vote(v3, &[(pair_hash, U256::in_units(500u64), SCALE_1E18)])
                .unwrap();

            crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

            // Median should be 50 (powers 100+200 cross threshold before 500)
            let (rate, _, _) = oracle.get_exchange_rate("COEN", "USDT").unwrap();
            assert_eq!(rate, U256::in_units(50u64));

            // v1 and v2 should be winners, v3 (outlier at 500) should miss
            assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 1);
            assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 1);
            assert_eq!(oracle.penalty_miss_count.read(&v3).unwrap(), 1);
        });
    }

    #[test]
    fn test_run_tally_no_votes_all_abstain() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let v1 = Address::new([0x11; 20]);
            register_validator(storage.clone(), v1, U256::in_units(100u64));

            // No votes submitted → all abstain
            crate::tally::run_tally(&mut oracle, 2, 24).unwrap();

            assert_eq!(oracle.penalty_abstain_count.read(&v1).unwrap(), 1);
            assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 0);
        });
    }

    #[test]
    fn test_hooks_begin_block() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let v1 = Address::new([0x11; 20]);
            register_validator(storage.clone(), v1, U256::in_units(100u64));

            let pair_hash = OracleContract::pair_hash("COEN", "USDT");
            oracle
                .submit_vote(v1, &[(pair_hash, U256::in_units(42u64), SCALE_1E18)])
                .unwrap();

            // Block 1: not a vote period boundary (period=2), no tally
            let runtime_ctx =
                BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 12, 1), storage.clone());
            <crate::hooks::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
            assert!(oracle.vote_exists.read(&v1).unwrap()); // vote still exists

            // Block 2: vote period boundary, tally runs
            let runtime_ctx =
                BlockRuntimeContext::new(BlockContext::empty_for_tests(2, 24, 1), storage.clone());
            <crate::hooks::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();
            assert!(!oracle.vote_exists.read(&v1).unwrap()); // votes cleared

            let (rate, _, _) = oracle.get_exchange_rate("COEN", "USDT").unwrap();
            assert_eq!(rate, U256::in_units(42u64));
        });
    }

    #[test]
    fn test_slash_window_processing() {
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
    fn test_slash_and_force_exit_failure_rolls_back_slash_state() {
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
    fn test_slash_failure_after_force_exit_rolls_back_exit_state() {
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
    fn test_protected_validator_not_force_exited() {
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

    // -----------------------------------------------------------------------
    // View functions
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_exchange_rates() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();
            oracle.register_pair("ETH", "USDT").unwrap();

            let rate1 = U256::from(1_500_000_000_000_000_000u128);
            let rate2 = U256::from(2_000_000_000_000_000_000u128);
            oracle
                .set_exchange_rate(Address::ZERO, "COEN", "USDT", rate1, 10, 120)
                .unwrap();
            oracle
                .set_exchange_rate(Address::ZERO, "ETH", "USDT", rate2, 20, 240)
                .unwrap();

            let (rates, blocks, timestamps) = oracle.get_exchange_rates().unwrap();
            assert_eq!(rates.len(), 2);
            assert_eq!(rates[0], rate1);
            assert_eq!(rates[1], rate2);
            assert_eq!(blocks[0], 10);
            assert_eq!(blocks[1], 20);
            assert_eq!(timestamps[0], 120);
            assert_eq!(timestamps[1], 240);
        });
    }

    #[test]
    fn test_get_exchange_rates_empty() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            let (rates, blocks, timestamps) = oracle.get_exchange_rates().unwrap();
            assert!(rates.is_empty());
            assert!(blocks.is_empty());
            assert!(timestamps.is_empty());
        });
    }

    #[test]
    fn test_get_vote_targets() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "USDT").unwrap();
            oracle.register_pair("ETH", "USDT").unwrap();
            oracle.register_pair("BTC", "USDT").unwrap();

            // Deactivate ETH/USDT (pair_id 2)
            oracle
                .deactivate_vote_target(Address::ZERO, "ETH", "USDT")
                .unwrap();

            let pair_ids = oracle.get_vote_targets().unwrap();
            assert_eq!(pair_ids, vec![1, 3]);
        });
    }

    #[test]
    fn test_get_vote_targets_empty() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            let pair_ids = oracle.get_vote_targets().unwrap();
            assert!(pair_ids.is_empty());
        });
    }

    #[test]
    fn test_get_aggregate_vote_exists() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();
            oracle.register_pair("ETH", "USDT").unwrap();

            let validator = Address::new([0x11; 20]);
            register_validator(storage.clone(), validator, U256::in_units(100u64));

            let hash1 = OracleContract::pair_hash("COEN", "USDT");
            let hash2 = OracleContract::pair_hash("ETH", "USDT");
            let rate1 = U256::in_units(50u64);
            let rate2 = U256::in_units(3000u64);
            let vol1 = U256::in_units(100u64);
            let vol2 = U256::in_units(200u64);

            oracle
                .submit_vote(validator, &[(hash1, rate1, vol1), (hash2, rate2, vol2)])
                .unwrap();

            let (exists, pair_ids, rates, volumes) = oracle.get_aggregate_vote(&validator).unwrap();
            assert!(exists);
            assert_eq!(pair_ids.len(), 2);
            assert_eq!(pair_ids[0], 1);
            assert_eq!(pair_ids[1], 2);
            assert_eq!(rates[0], rate1);
            assert_eq!(rates[1], rate2);
            assert_eq!(volumes[0], vol1);
            assert_eq!(volumes[1], vol2);
        });
    }

    #[test]
    fn test_get_aggregate_vote_not_exists() {
        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            let validator = Address::new([0x11; 20]);

            let (exists, pair_ids, rates, volumes) = oracle.get_aggregate_vote(&validator).unwrap();
            assert!(!exists);
            assert!(pair_ids.is_empty());
            assert!(rates.is_empty());
            assert!(volumes.is_empty());
        });
    }

    #[test]
    fn test_get_slash_window_progress() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);

            let validator = Address::new([0x11; 20]);

            oracle.increment_success(&validator).unwrap();
            oracle.increment_success(&validator).unwrap();
            oracle.increment_abstain(&validator).unwrap();
            oracle.increment_miss(&validator).unwrap();
            oracle.increment_miss(&validator).unwrap();
            oracle.increment_miss(&validator).unwrap();

            let (success, abstain, miss, slash_window) =
                oracle.get_slash_window_progress(&validator).unwrap();
            assert_eq!(success, 2);
            assert_eq!(abstain, 1);
            assert_eq!(miss, 3);
            assert_eq!(slash_window, 96); // from init_oracle
        });
    }

    // -----------------------------------------------------------------------
    // Genesis config tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_init_from_genesis_default_matches_hardcoded() {
        with_storage(|storage| {
            // Reference: manually-written init (mirrors the old executor code).
            let mut expected = OracleContract::new(storage.clone());
            expected.config_vote_period.write(2).unwrap();
            expected
                .config_reward_band
                .write(U256::from(20_000_000_000_000_000u128))
                .unwrap();
            expected.config_slash_window.write(96).unwrap();
            expected
                .config_min_valid_per_window
                .write(U256::from(50_000_000_000_000_000u128))
                .unwrap();
            expected.config_slash_fraction.write(U256::ZERO).unwrap();
            expected.config_lookback_duration.write(86400).unwrap();
            expected.config_enabled.write(true).unwrap();
            expected.config_is_initialized.write(true).unwrap();
            expected.register_pair("COEN", "0xUSD").unwrap();

            // Snapshot expected state.
            let exp_vote_period = expected.config_vote_period.read().unwrap();
            let exp_reward_band = expected.config_reward_band.read().unwrap();
            let exp_slash_window = expected.config_slash_window.read().unwrap();
            let exp_min_valid = expected.config_min_valid_per_window.read().unwrap();
            let exp_slash_fraction = expected.config_slash_fraction.read().unwrap();
            let exp_lookback = expected.config_lookback_duration.read().unwrap();
            let exp_enabled = expected.config_enabled.read().unwrap();
            let exp_initialized = expected.config_is_initialized.read().unwrap();
            let exp_pair_count = expected.pair_count.read().unwrap();
            let exp_pair_id = expected.get_pair_id("COEN", "0xUSD").unwrap();
            let exp_vote_target = expected.is_vote_target("COEN", "0xUSD").unwrap();

            // Now init through the genesis config path (uses same storage).
            // Since pairs are already registered we need a fresh storage context.
            // We re-enter with a new provider to start clean.

            let mut storage2 = HashMapStorageProvider::new(2);
            StorageHandle::enter(&mut storage2, |storage| {
                let mut oracle = OracleContract::new(storage.clone());
                crate::logic::init_from_genesis(
                    &mut oracle,
                    &crate::logic::OracleGenesisConfig::default_config(),
                )
                .unwrap();

                assert_eq!(oracle.config_vote_period.read().unwrap(), exp_vote_period);
                assert_eq!(oracle.config_reward_band.read().unwrap(), exp_reward_band);
                assert_eq!(oracle.config_slash_window.read().unwrap(), exp_slash_window);
                assert_eq!(
                    oracle.config_min_valid_per_window.read().unwrap(),
                    exp_min_valid
                );
                assert_eq!(
                    oracle.config_slash_fraction.read().unwrap(),
                    exp_slash_fraction
                );
                assert_eq!(
                    oracle.config_lookback_duration.read().unwrap(),
                    exp_lookback
                );
                assert_eq!(oracle.config_enabled.read().unwrap(), exp_enabled);
                assert_eq!(
                    oracle.config_is_initialized.read().unwrap(),
                    exp_initialized
                );
                assert_eq!(oracle.pair_count.read().unwrap(), exp_pair_count);
                assert_eq!(oracle.get_pair_id("COEN", "0xUSD").unwrap(), exp_pair_id);
                assert_eq!(
                    oracle.is_vote_target("COEN", "0xUSD").unwrap(),
                    exp_vote_target
                );
            });
        });
    }

    #[test]
    fn test_init_from_genesis_custom_config() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                vote_period: 5,
                reward_band: U256::from(10_000_000_000_000_000u128), // 0.01
                slash_window: 200,
                min_valid_per_window: U256::from(100_000_000_000_000_000u128), // 0.10
                slash_fraction: U256::from(1_000_000_000_000_000u128),         // 0.001
                lookback_duration: 172_800,                                    // 2 days
                pairs: vec![
                    ("COEN".into(), "0xUSD".into()),
                    ("ETH".into(), "0xUSD".into()),
                    ("BTC".into(), "USDT".into()),
                ],
                initial_rates: vec![
                    ("COEN".into(), "0xUSD".into(), U256::in_units(1u64)),
                    ("ETH".into(), "0xUSD".into(), U256::in_units(2000u64)),
                ],
                feeder_delegations: vec![
                    (Address::new([0x11; 20]), Address::new([0xAAu8; 20])),
                    (Address::new([0x22; 20]), Address::new([0xBBu8; 20])),
                ],
                settlement_currencies: vec![
                    (840, "0xUSD".into(), "COEN".into(), "0xUSD".into()),
                    (978, "EURC".into(), "ETH".into(), "0xUSD".into()),
                ],
                reference_currencies: vec![840],
                penalty_counters: vec![],
                aggregate_votes: vec![],
                snapshots: vec![],
                scurve_entries: vec![],
                protected_validators: vec![],
            };

            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            // Verify config slots.
            assert_eq!(oracle.config_vote_period.read().unwrap(), 5);
            assert_eq!(
                oracle.config_reward_band.read().unwrap(),
                U256::from(10_000_000_000_000_000u128)
            );
            assert_eq!(oracle.config_slash_window.read().unwrap(), 200);
            assert_eq!(
                oracle.config_min_valid_per_window.read().unwrap(),
                U256::from(100_000_000_000_000_000u128)
            );
            assert_eq!(
                oracle.config_slash_fraction.read().unwrap(),
                U256::from(1_000_000_000_000_000u128)
            );
            assert_eq!(oracle.config_lookback_duration.read().unwrap(), 172_800);
            assert!(oracle.config_enabled.read().unwrap());
            assert!(oracle.config_is_initialized.read().unwrap());

            // Verify all three pairs registered.
            assert_eq!(oracle.pair_count.read().unwrap(), 3);
            assert_eq!(oracle.get_pair_id("COEN", "0xUSD").unwrap(), 1);
            assert_eq!(oracle.get_pair_id("ETH", "0xUSD").unwrap(), 2);
            assert_eq!(oracle.get_pair_id("BTC", "USDT").unwrap(), 3);
            assert!(oracle.is_vote_target("COEN", "0xUSD").unwrap());
            assert!(oracle.is_vote_target("ETH", "0xUSD").unwrap());
            assert!(oracle.is_vote_target("BTC", "USDT").unwrap());

            // Verify initial rates (only first two pairs have rates).
            let (rate_coen, blk, ts) = oracle.get_exchange_rate("COEN", "0xUSD").unwrap();
            assert_eq!(rate_coen, U256::in_units(1u64));
            assert_eq!(blk, 0);
            assert_eq!(ts, 0);

            let (rate_eth, _, _) = oracle.get_exchange_rate("ETH", "0xUSD").unwrap();
            assert_eq!(rate_eth, U256::in_units(2000u64));

            // BTC/USDT has no initial rate set → zero.
            let (rate_btc, _, _) = oracle.get_exchange_rate("BTC", "USDT").unwrap();
            assert_eq!(rate_btc, U256::ZERO);

            // Verify feeder delegations.
            let v1 = Address::new([0x11; 20]);
            let v2 = Address::new([0x22; 20]);
            assert_eq!(oracle.get_feeder(&v1).unwrap(), Address::new([0xAAu8; 20]));
            assert_eq!(oracle.get_feeder(&v2).unwrap(), Address::new([0xBBu8; 20]));

            // Verify settlement currencies are indexed and reversible.
            assert_eq!(oracle.settlement_count.read().unwrap(), 2);
            assert_eq!(oracle.settlement_index_to_iso.read(&0).unwrap(), 840);
            assert_eq!(oracle.settlement_index_to_iso.read(&1).unwrap(), 978);
            assert_eq!(
                oracle
                    .settlement_iso_to_denom_string
                    .read_string(&840)
                    .unwrap(),
                "0xUSD"
            );
            assert_eq!(
                oracle
                    .settlement_iso_to_denom_string
                    .read_string(&978)
                    .unwrap(),
                "EURC"
            );
        });
    }

    #[test]
    fn test_init_from_genesis_idempotent() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let config = crate::logic::OracleGenesisConfig::default_config();

            // First init succeeds
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();
            assert!(oracle.config_is_initialized.read().unwrap());
            assert_eq!(oracle.pair_count.read().unwrap(), 1);

            // Second init is a no-op (idempotent — no error)
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();
            assert_eq!(oracle.pair_count.read().unwrap(), 1); // still 1, not 2
        });
    }

    #[test]
    fn test_feeder_delegation() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "USDT").unwrap();

            let validator = Address::new([0x11; 20]);
            let feeder = Address::new([0x22; 20]);
            register_validator(storage.clone(), validator, U256::in_units(100u64));

            // Delegate
            oracle.delegate_feeder(validator, feeder).unwrap();
            assert_eq!(oracle.get_feeder(&validator).unwrap(), feeder);

            // Feeder can submit vote on behalf of validator
            let pair_hash = OracleContract::pair_hash("COEN", "USDT");
            oracle
                .submit_vote(feeder, &[(pair_hash, U256::in_units(50u64), SCALE_1E18)])
                .unwrap();

            assert!(oracle.vote_exists.read(&validator).unwrap());
        });
    }

    #[test]
    fn test_precompile_dispatch_get_params() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);

            // Encode getParams() call
            use crate::precompile::IOracle;
            use alloy_sol_types::SolCall;
            let calldata = IOracle::getParamsCall {}.abi_encode();

            // Dispatch through precompile
            let result =
                crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                    .unwrap();

            // Decode result
            let decoded = IOracle::getParamsCall::abi_decode_returns(&result).unwrap();
            assert_eq!(decoded.votePeriod, 2);
            assert!(decoded.enabled);
        });
    }

    #[test]
    fn test_precompile_dispatch_get_exchange_rate_round_trip() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            oracle.register_pair("COEN", "0xUSD").unwrap();
            let expected_rate = U256::in_units(123u64);
            oracle
                .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", expected_rate, 42, 86_400)
                .unwrap();

            use crate::precompile::IOracle;
            use alloy_sol_types::SolCall;

            let calldata = IOracle::getExchangeRateCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
            }
            .abi_encode();
            let result =
                crate::precompile::dispatch(storage.clone(), &calldata, Address::ZERO, U256::ZERO)
                    .unwrap();
            let decoded = IOracle::getExchangeRateCall::abi_decode_returns(&result).unwrap();

            assert_eq!(decoded.rate, expected_rate);
            assert_eq!(decoded.lastBlock, 42);
            assert_eq!(decoded.lastTimestamp, 86_400);
        });
    }

    #[test]
    fn test_scurve_hook_integration_detects_daily_peak() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);
            let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();

            let day_1 = crate::scurve::DAY_SECONDS;
            let day_2 = 2 * crate::scurve::DAY_SECONDS;
            let day_3 = 3 * crate::scurve::DAY_SECONDS;
            let day_4 = 4 * crate::scurve::DAY_SECONDS;
            // Three fully-closed days forming a peak at day_2: 100 < 150 > 120.
            oracle
                .write_snapshot(day_1 + 60, &[(pair_id, U256::in_units(100u64), SCALE_1E18)])
                .unwrap();
            oracle
                .write_snapshot(day_2 + 60, &[(pair_id, U256::in_units(150u64), SCALE_1E18)])
                .unwrap();
            oracle
                .write_snapshot(day_3 + 60, &[(pair_id, U256::in_units(120u64), SCALE_1E18)])
                .unwrap();

            // Hook fires on the first block of day_4 — the current day has NO
            // close yet, mirroring the real start-of-day boundary block.
            let runtime_ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(4, day_4 + 120, 1),
                storage.clone(),
            );
            <crate::hooks::OracleLifecycle as BlockLifecycle>::begin_block(&runtime_ctx).unwrap();

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
    fn test_precompile_dispatch_cosmos_query_surface_round_trips() {
        with_storage_at(3_000, |storage| {
            let mut oracle = OracleContract::new(storage.clone());
            init_oracle(&mut oracle);

            let coen_id = oracle.register_pair("COEN", "0xUSD").unwrap();
            let eth_id = oracle.register_pair("ETH", "0xUSD").unwrap();
            oracle
                .write_snapshot(
                    1_000,
                    &[
                        (coen_id, U256::in_units(100u64), SCALE_1E18),
                        (eth_id, U256::in_units(2_000u64), SCALE_1E18),
                    ],
                )
                .unwrap();
            oracle
                .write_snapshot(
                    2_000,
                    &[
                        (coen_id, U256::in_units(120u64), SCALE_1E18),
                        (eth_id, U256::in_units(2_200u64), SCALE_1E18),
                    ],
                )
                .unwrap();
            oracle
                .write_snapshot(
                    3_000,
                    &[
                        (coen_id, U256::in_units(140u64), SCALE_1E18),
                        (eth_id, U256::in_units(2_400u64), SCALE_1E18),
                    ],
                )
                .unwrap();

            crate::scurve::store_scurve_entry(&mut oracle, coen_id, 0, U256::in_units(160u64))
                .unwrap();

            let denom_hash = alloy_primitives::keccak256("0xUSD".as_bytes());
            let pair_hash = OracleContract::pair_hash("COEN", "0xUSD");
            oracle.settlement_count.write(1).unwrap();
            oracle.settlement_index_to_iso.write(&0, 840).unwrap();
            oracle
                .settlement_iso_to_denom
                .write(&840, denom_hash)
                .unwrap();
            oracle
                .settlement_iso_to_pair
                .write(&840, pair_hash)
                .unwrap();
            oracle
                .settlement_iso_to_denom_string
                .write_string(&840, "0xUSD")
                .unwrap();

            use crate::precompile::IOracle;
            use alloy_sol_types::SolCall;

            let history = IOracle::getAllPriceSnapshotHistoryCall { count: 2 }.abi_encode();
            let decoded = IOracle::getAllPriceSnapshotHistoryCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &history, Address::ZERO, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.snapshotIds, vec![2, 2, 1, 1]);
            assert_eq!(decoded.pairIds, vec![coen_id, eth_id, coen_id, eth_id]);

            let twaps = IOracle::getTwapsCall { lookback: 2_500 }.abi_encode();
            let decoded = IOracle::getTwapsCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &twaps, Address::ZERO, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.pairIds, vec![coen_id, eth_id]);
            assert_eq!(decoded.lookbackSeconds, vec![2_500, 2_500]);
            assert_eq!(decoded.twaps.len(), 2);

            let wwd = IOracle::getWorldwideDayVwapCall {
                startTime: 1_000,
                endTime: 3_000,
            }
            .abi_encode();
            let decoded = IOracle::getWorldwideDayVwapCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &wwd, Address::ZERO, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.pairIds, vec![coen_id, eth_id]);
            assert_eq!(decoded.lookbackSeconds, vec![2_000, 2_000]);

            let scurve_values = IOracle::getScurveValuesCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
                timestamp: 3_000,
            }
            .abi_encode();
            let decoded = IOracle::getScurveValuesCall::abi_decode_returns(
                &crate::precompile::dispatch(
                    storage.clone(),
                    &scurve_values,
                    Address::ZERO,
                    U256::ZERO,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.targetDay, 0);
            assert_eq!(decoded.peakDays, vec![0]);
            assert_eq!(decoded.values, vec![U256::in_units(160u64)]);

            let scurve_data = IOracle::getAllScurveDataForPairCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
            }
            .abi_encode();
            let decoded = IOracle::getAllScurveDataForPairCall::abi_decode_returns(
                &crate::precompile::dispatch(
                    storage.clone(),
                    &scurve_data,
                    Address::ZERO,
                    U256::ZERO,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.peakDays, vec![0]);
            assert_eq!(decoded.peakPrices, vec![U256::in_units(160u64)]);

            let settlements = IOracle::getSettlementCurrenciesCall {}.abi_encode();
            let decoded = IOracle::getSettlementCurrenciesCall::abi_decode_returns(
                &crate::precompile::dispatch(
                    storage.clone(),
                    &settlements,
                    Address::ZERO,
                    U256::ZERO,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.isoCodes, vec![840]);
            assert_eq!(decoded.denoms, vec!["0xUSD".to_string()]);
            assert_eq!(decoded.denomHashes, vec![denom_hash]);
            assert_eq!(decoded.pairHashes, vec![pair_hash]);

            let nominal_components = IOracle::getNominalPriceComponentsCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
                timestamp: 3_000,
            }
            .abi_encode();
            let decoded = IOracle::getNominalPriceComponentsCall::abi_decode_returns(
                &crate::precompile::dispatch(
                    storage.clone(),
                    &nominal_components,
                    Address::ZERO,
                    U256::ZERO,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.nominalPrice, U256::in_units(160u64));
            assert_eq!(decoded.maxScurve, U256::in_units(160u64));
            assert_eq!(decoded.source, "scurve");

            let nominal = IOracle::getNominalPriceCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
                timestamp: 3_000,
            }
            .abi_encode();
            let decoded = IOracle::getNominalPriceCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &nominal, Address::ZERO, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded, U256::in_units(160u64));
        });
    }

    #[test]
    fn test_ioracle_selectors_unique() {
        use crate::precompile::IOracle;
        use alloy_sol_types::SolInterface;
        use std::collections::HashSet;

        const EXPECTED_IORACLE_FUNCTIONS: usize = 37;

        let selectors: Vec<[u8; 4]> = IOracle::IOracleCalls::selectors().collect();
        assert_eq!(
            selectors.len(),
            IOracle::IOracleCalls::COUNT,
            "selector iterator must cover every generated IOracle call variant"
        );
        assert_eq!(
            IOracle::IOracleCalls::COUNT,
            EXPECTED_IORACLE_FUNCTIONS,
            "IOracle function count changed; update selector collision coverage"
        );

        let external_interface_count =
            include_str!("../../../../contracts/precompiles/src/IOracle.sol")
                .lines()
                .filter(|line| line.trim_start().starts_with("function "))
                .count();
        assert_eq!(
            external_interface_count, EXPECTED_IORACLE_FUNCTIONS,
            "contracts/precompiles/src/IOracle.sol function count must stay in sync with precompile IOracle"
        );

        let unique: HashSet<[u8; 4]> = selectors.iter().copied().collect();
        assert_eq!(
            unique.len(),
            selectors.len(),
            "selector collision detected among {} IOracle functions",
            selectors.len()
        );
    }

    // -----------------------------------------------------------------------
    // Genesis Import: Snapshots, Penalties, S-curves
    // -----------------------------------------------------------------------

    #[test]
    fn test_genesis_import_penalty_counters() {
        with_storage(|storage| {
            let v1 = Address::new([0x11; 20]);
            let v2 = Address::new([0x22; 20]);
            let config = crate::logic::OracleGenesisConfig {
                penalty_counters: vec![(v1, 10, 2, 3), (v2, 5, 0, 1)],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 10);
            assert_eq!(oracle.penalty_abstain_count.read(&v1).unwrap(), 2);
            assert_eq!(oracle.penalty_miss_count.read(&v1).unwrap(), 3);
            assert_eq!(oracle.penalty_success_count.read(&v2).unwrap(), 5);
            assert_eq!(oracle.penalty_miss_count.read(&v2).unwrap(), 1);
        });
    }

    #[test]
    fn test_genesis_import_snapshots() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                snapshots: vec![
                    crate::logic::GenesisSnapshot {
                        timestamp: 1000,
                        entries: vec![(1, U256::in_units(100u64), SCALE_1E18)],
                    },
                    crate::logic::GenesisSnapshot {
                        timestamp: 2000,
                        entries: vec![(1, U256::in_units(200u64), SCALE_1E18)],
                    },
                ],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 2);
            assert_eq!(oracle.snapshot_timestamp.read(&0u64).unwrap(), 1000);
            assert_eq!(oracle.snapshot_timestamp.read(&1u64).unwrap(), 2000);
        });
    }

    #[test]
    fn test_genesis_import_scurve_entries() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                scurve_entries: vec![
                    crate::logic::GenesisScurveEntry {
                        pair_id: 1,
                        peak_day: 86400,
                        peak_price: U256::in_units(500u64),
                    },
                    crate::logic::GenesisScurveEntry {
                        pair_id: 1,
                        peak_day: 86400 * 10,
                        peak_price: U256::in_units(600u64),
                    },
                ],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            assert_eq!(oracle.scurve_count.read().unwrap(), 2);
            assert_eq!(oracle.scurve_pair_id.read(&0u32).unwrap(), 1);
            assert_eq!(oracle.scurve_peak_day.read(&0u32).unwrap(), 86400);
            assert_eq!(
                oracle.scurve_peak_price.read(&0u32).unwrap(),
                U256::in_units(500u64)
            );
        });
    }

    #[test]
    fn test_genesis_import_protected_validators() {
        with_storage(|storage| {
            let v1 = Address::new([0x11; 20]);
            let v2 = Address::new([0x22; 20]);
            let config = crate::logic::OracleGenesisConfig {
                protected_validators: vec![v1, v2],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            assert!(oracle.config_allow_protected.read().unwrap());
            assert!(oracle.protected_validator.read(&v1).unwrap());
            assert!(oracle.protected_validator.read(&v2).unwrap());
            assert!(!oracle
                .protected_validator
                .read(&Address::new([0x33; 20]))
                .unwrap());
        });
    }

    #[test]
    fn test_genesis_import_aggregate_votes() {
        with_storage(|storage| {
            let validator = Address::new([0x11; 20]);
            let rate1 = U256::in_units(42u64);
            let rate2 = U256::in_units(2100u64);
            let volume1 = U256::in_units(100u64);
            let volume2 = U256::in_units(200u64);
            let config = crate::logic::OracleGenesisConfig {
                pairs: vec![
                    ("COEN".into(), "0xUSD".into()),
                    ("ETH".into(), "0xUSD".into()),
                ],
                aggregate_votes: vec![crate::logic::GenesisAggregateVote {
                    validator,
                    entries: vec![(1, rate1, volume1), (2, rate2, volume2)],
                }],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            assert!(oracle.vote_exists.read(&validator).unwrap());
            assert_eq!(oracle.vote_tuple_count.read(&validator).unwrap(), 2);
            assert_eq!(oracle.voter_list.len().unwrap(), 1);
            assert_eq!(oracle.voter_list.get(0).unwrap(), Some(validator));

            let (exists, pair_ids, rates, volumes) = oracle.get_aggregate_vote(&validator).unwrap();
            assert!(exists);
            assert_eq!(pair_ids, vec![1, 2]);
            assert_eq!(rates, vec![rate1, rate2]);
            assert_eq!(volumes, vec![volume1, volume2]);
        });
    }

    #[test]
    fn test_genesis_rejects_duplicate_aggregate_vote_pair() {
        with_storage(|storage| {
            let validator = Address::new([0x11; 20]);
            let config = crate::logic::OracleGenesisConfig {
                aggregate_votes: vec![crate::logic::GenesisAggregateVote {
                    validator,
                    entries: vec![
                        (1, U256::in_units(42u64), SCALE_1E18),
                        (1, U256::in_units(43u64), SCALE_1E18),
                    ],
                }],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            assert!(crate::logic::init_from_genesis(&mut oracle, &config).is_err());
            assert!(!oracle.config_is_initialized.read().unwrap());
            assert!(!oracle.config_enabled.read().unwrap());
            assert!(!oracle.vote_exists.read(&validator).unwrap());
            assert_eq!(oracle.voter_list.len().unwrap(), 0);
        });
    }

    #[test]
    fn test_genesis_rejects_duplicate_settlement_iso_code() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                settlement_currencies: vec![
                    (840, "0xUSD".into(), "COEN".into(), "0xUSD".into()),
                    (840, "USDC".into(), "COEN".into(), "0xUSD".into()),
                ],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            assert!(crate::logic::init_from_genesis(&mut oracle, &config).is_err());
        });
    }

    #[test]
    fn test_genesis_rejects_unregistered_settlement_pair() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                settlement_currencies: vec![(840, "0xUSD".into(), "ETH".into(), "0xUSD".into())],
                ..crate::logic::OracleGenesisConfig::default_config()
            };

            let mut oracle = OracleContract::new(storage.clone());
            assert!(crate::logic::init_from_genesis(&mut oracle, &config).is_err());
        });
    }

    #[test]
    fn test_genesis_export_round_trip() {
        let v1 = Address::new([0x11; 20]);
        let v2 = Address::new([0x22; 20]);
        let config = crate::logic::OracleGenesisConfig {
            pairs: vec![
                ("COEN".into(), "0xUSD".into()),
                ("ETH".into(), "0xUSD".into()),
                ("BTC".into(), "USDT".into()),
            ],
            initial_rates: vec![
                ("COEN".into(), "0xUSD".into(), U256::in_units(1u64)),
                ("ETH".into(), "0xUSD".into(), U256::in_units(2000u64)),
            ],
            feeder_delegations: vec![(v1, Address::new([0xAAu8; 20]))],
            aggregate_votes: vec![
                crate::logic::GenesisAggregateVote {
                    validator: v1,
                    entries: vec![
                        (1, U256::in_units(42u64), SCALE_1E18),
                        (2, U256::in_units(2100u64), SCALE_1E18),
                    ],
                },
                crate::logic::GenesisAggregateVote {
                    validator: v2,
                    entries: vec![(1, U256::in_units(41u64), SCALE_1E18)],
                },
            ],
            settlement_currencies: vec![
                (840, "0xUSD".into(), "COEN".into(), "0xUSD".into()),
                (978, "EURC".into(), "ETH".into(), "0xUSD".into()),
            ],
            reference_currencies: vec![840, 978],
            penalty_counters: vec![(v1, 7, 2, 1), (v2, 3, 0, 4)],
            snapshots: vec![crate::logic::GenesisSnapshot {
                timestamp: 5000,
                entries: vec![
                    (1, U256::in_units(42u64), SCALE_1E18),
                    (2, U256::in_units(2100u64), SCALE_1E18),
                ],
            }],
            scurve_entries: vec![crate::logic::GenesisScurveEntry {
                pair_id: 1,
                peak_day: 86400,
                peak_price: U256::in_units(100u64),
            }],
            protected_validators: vec![v1],
            vote_period: 2,
            reward_band: U256::from(20_000_000_000_000_000u128),
            slash_window: 96,
            min_valid_per_window: U256::from(50_000_000_000_000_000u128),
            slash_fraction: U256::ZERO,
            lookback_duration: 86400,
        };

        let exported = {
            let mut storage = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut storage, |storage| {
                let mut oracle = OracleContract::new(storage.clone());
                crate::logic::init_from_genesis(&mut oracle, &config).unwrap();
                crate::logic::export_genesis(&oracle, &[v1, v2]).unwrap()
            })
        };

        assert_eq!(exported.vote_period, 2);
        assert_eq!(exported.slash_window, 96);
        assert_eq!(exported.pairs, config.pairs);
        assert_eq!(exported.initial_rates, config.initial_rates);
        assert_eq!(exported.feeder_delegations, config.feeder_delegations);
        assert_eq!(exported.aggregate_votes.len(), 2);
        assert_eq!(exported.aggregate_votes[0].validator, v1);
        assert_eq!(
            exported.aggregate_votes[0].entries,
            config.aggregate_votes[0].entries
        );
        assert_eq!(exported.aggregate_votes[1].validator, v2);
        assert_eq!(
            exported.aggregate_votes[1].entries,
            config.aggregate_votes[1].entries
        );
        assert_eq!(exported.settlement_currencies, config.settlement_currencies);
        assert_eq!(exported.penalty_counters, config.penalty_counters);
        assert_eq!(exported.snapshots.len(), 1);
        assert_eq!(exported.snapshots[0].timestamp, 5000);
        assert_eq!(exported.snapshots[0].entries.len(), 2);
        assert_eq!(exported.scurve_entries.len(), 1);
        assert_eq!(exported.scurve_entries[0].pair_id, 1);
        assert_eq!(exported.scurve_entries[0].peak_day, 86400);
        assert_eq!(exported.protected_validators, vec![v1]);

        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &exported).unwrap();

            assert_eq!(oracle.pair_count.read().unwrap(), 3);
            assert_eq!(oracle.get_pair_id("COEN", "0xUSD").unwrap(), 1);
            assert_eq!(oracle.get_pair_id("ETH", "0xUSD").unwrap(), 2);
            assert_eq!(oracle.get_pair_id("BTC", "USDT").unwrap(), 3);
            assert_eq!(
                oracle.get_exchange_rate("COEN", "0xUSD").unwrap().0,
                U256::in_units(1u64)
            );
            assert_eq!(
                oracle.get_exchange_rate("ETH", "0xUSD").unwrap().0,
                U256::in_units(2000u64)
            );
            assert_eq!(oracle.get_feeder(&v1).unwrap(), Address::new([0xAAu8; 20]));
            assert_eq!(oracle.get_aggregate_vote(&v1).unwrap().1, vec![1, 2]);
            assert_eq!(oracle.get_aggregate_vote(&v2).unwrap().1, vec![1]);
            assert_eq!(oracle.settlement_count.read().unwrap(), 2);
            assert_eq!(
                oracle
                    .settlement_iso_to_denom_string
                    .read_string(&840)
                    .unwrap(),
                "0xUSD"
            );
            assert_eq!(
                oracle
                    .settlement_iso_to_denom_string
                    .read_string(&978)
                    .unwrap(),
                "EURC"
            );
            assert_eq!(oracle.penalty_success_count.read(&v1).unwrap(), 7);
            assert_eq!(oracle.penalty_miss_count.read(&v2).unwrap(), 4);
            assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 1);
            assert_eq!(oracle.scurve_count.read().unwrap(), 1);
            assert!(oracle.protected_validator.read(&v1).unwrap());
        });
    }

    #[test]
    fn test_genesis_export_fails_without_pair_string_metadata() {
        with_storage(|storage| {
            let hash = OracleContract::pair_hash("COEN", "0xUSD");
            let oracle = OracleContract::new(storage.clone());
            oracle.pair_count.write(1).unwrap();
            oracle.pair_id_to_hash.write(&1, hash).unwrap();
            oracle.pair_hash_to_id.write(&hash, 1).unwrap();

            assert!(crate::logic::export_genesis(&oracle, &[]).is_err());
        });
    }

    #[test]
    fn test_genesis_export_fails_without_settlement_denom_metadata() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                settlement_currencies: vec![(840, "0xUSD".into(), "COEN".into(), "0xUSD".into())],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();
            oracle
                .settlement_iso_to_denom_string
                .get_bytes(&840)
                .clear()
                .unwrap();

            assert!(crate::logic::export_genesis(&oracle, &[]).is_err());
        });
    }

    #[test]
    fn test_genesis_export_omits_zero_initial_rate() {
        with_storage(|storage| {
            let config = crate::logic::OracleGenesisConfig {
                pairs: vec![
                    ("COEN".into(), "0xUSD".into()),
                    ("BTC".into(), "USDT".into()),
                ],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            let exported = crate::logic::export_genesis(&oracle, &[]).unwrap();
            assert_eq!(exported.pairs, config.pairs);
            assert!(exported.initial_rates.is_empty());
        });
    }
    #[test]
    fn test_store_and_query_worldwide_day_vwap_snapshot() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let coen_id = oracle.register_pair("COEN", "0xUSD").unwrap();
            let eth_id = oracle.register_pair("ETH", "0xUSD").unwrap();

            oracle
                .write_snapshot(
                    1_500,
                    &[
                        (coen_id, U256::from(110u64), U256::from(1u64)),
                        (eth_id, U256::from(2_200u64), U256::from(1u64)),
                    ],
                )
                .unwrap();

            oracle
                .store_worldwide_day_vwap_snapshot(20260302u32.into(), 1_000, 3_000)
                .unwrap();

            let (start_time, end_time, pair_ids, vwaps, lookbacks) = oracle
                .get_worldwide_day_vwap_snapshot(20260302u32.into())
                .unwrap();
            assert_eq!(start_time, 1_000);
            assert_eq!(end_time, 3_000);
            assert_eq!(pair_ids, vec![coen_id, eth_id]);
            assert_eq!(vwaps, vec![U256::from(110u64), U256::from(2_200u64)]);
            assert_eq!(lookbacks, vec![2_000, 2_000]);
            assert_eq!(
                oracle
                    .get_worldwide_day_vwap_for_pair_id(20260302u32.into(), coen_id)
                    .unwrap(),
                Some(U256::from(110u64))
            );

            use crate::precompile::IOracle;
            use alloy_sol_types::SolCall;

            let call = IOracle::getWorldwideDayVwapSnapshotCall {
                worldwideDay: 20260302,
            }
            .abi_encode();
            let decoded = IOracle::getWorldwideDayVwapSnapshotCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.startTime, 1_000);
            assert_eq!(decoded.endTime, 3_000);
            assert_eq!(decoded.pairIds, vec![coen_id, eth_id]);
            assert_eq!(
                decoded.vwaps,
                vec![U256::from(110u64), U256::from(2_200u64)]
            );
        });
    }

    #[test]
    fn test_api_day_type_pair_vwap_and_snapshot_store() {
        with_storage(|storage| {
            let wwd = outbe_common::WorldwideDay::new(20260302u32);

            // Pair not registered yet → typed None, not an error.
            assert_eq!(
                crate::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
                None
            );

            let mut oracle = OracleContract::new(storage.clone());
            oracle.register_pair("COEN", "0xUSD").unwrap();
            oracle
                .write_snapshot(1_500, &[(1, U256::from(110u64), U256::from(1u64))])
                .unwrap();

            // No window data → store is a deterministic no-op returning false,
            // not a "no VWAP data" revert leaking to the caller.
            assert!(
                !crate::api::store_worldwide_day_vwap_snapshot(storage.clone(), wwd, 100, 200)
                    .unwrap()
            );
            assert_eq!(
                crate::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
                None,
                "no snapshot written → None"
            );

            // Window with data → store writes (true) and the COEN VWAP resolves.
            assert!(crate::api::store_worldwide_day_vwap_snapshot(
                storage.clone(),
                wwd,
                1_000,
                3_000
            )
            .unwrap());
            assert_eq!(
                crate::api::day_type_pair_vwap(storage.clone(), wwd).unwrap(),
                Some(U256::from(110u64))
            );
        });
    }

    #[test]
    fn test_finalize_utc_day_vwap_writes_and_reads() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let coen = oracle.register_pair("COEN", "0xUSD").unwrap();
            let eth = oracle.register_pair("ETH", "0xUSD").unwrap();

            let utc_day = 20260624u32;
            let day_start = outbe_primitives::time::date_key_to_utc_timestamp(utc_day);

            // Two COEN samples within the day → volume-weighted:
            // (100*2 + 200*1) / (2 + 1) = 400 / 3 = 133.
            oracle
                .write_snapshot(
                    day_start + 100,
                    &[(coen, U256::from(100u64), U256::from(2u64))],
                )
                .unwrap();
            oracle
                .write_snapshot(
                    day_start + 200,
                    &[(coen, U256::from(200u64), U256::from(1u64))],
                )
                .unwrap();
            // ETH single sample → VWAP == rate.
            oracle
                .write_snapshot(
                    day_start + 300,
                    &[(eth, U256::from(2_200u64), U256::from(1u64))],
                )
                .unwrap();

            oracle.finalize_utc_day_vwap(utc_day).unwrap();

            assert_eq!(oracle.utc_day_vwap_pair_count.read(&utc_day).unwrap(), 2);
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(utc_day, coen).unwrap(),
                Some(U256::from(133u64))
            );
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(utc_day, eth).unwrap(),
                Some(U256::from(2_200u64))
            );

            let (pairs, vwaps) = oracle.get_utc_day_vwap_snapshot(utc_day).unwrap();
            assert_eq!(pairs, vec![coen, eth]);
            assert_eq!(vwaps, vec![U256::from(133u64), U256::from(2_200u64)]);

            // Unknown pair on a finalized day, and an unfinalized day, both read None.
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(utc_day, 999).unwrap(),
                None
            );
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(20260101, coen).unwrap(),
                None
            );
        });
    }

    #[test]
    fn test_finalize_empty_utc_day_writes_nothing() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let coen = oracle.register_pair("COEN", "0xUSD").unwrap();
            let utc_day = 20260624u32;

            // No snapshots for the day → finalize is a no-op, nothing written.
            oracle.finalize_utc_day_vwap(utc_day).unwrap();

            assert_eq!(oracle.utc_day_vwap_pair_count.read(&utc_day).unwrap(), 0);
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(utc_day, coen).unwrap(),
                None
            );
            let (pairs, vwaps) = oracle.get_utc_day_vwap_snapshot(utc_day).unwrap();
            assert!(pairs.is_empty() && vwaps.is_empty());
        });
    }

    #[test]
    fn test_get_utc_day_vwap_precompile() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let coen = oracle.register_pair("COEN", "0xUSD").unwrap();
            let utc_day = 20260624u32;
            let day_start = outbe_primitives::time::date_key_to_utc_timestamp(utc_day);
            oracle
                .write_snapshot(
                    day_start + 100,
                    &[(coen, U256::from(150u64), U256::from(1u64))],
                )
                .unwrap();
            oracle.finalize_utc_day_vwap(utc_day).unwrap();

            use crate::precompile::IOracle;
            use alloy_sol_types::SolCall;

            // Finalized day → returns the stored VWAP.
            let call = IOracle::getUtcDayVwapCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
                utcDay: utc_day,
            }
            .abi_encode();
            let decoded = IOracle::getUtcDayVwapCall::abi_decode_returns(
                &crate::precompile::dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(decoded, U256::from(150u64));

            // Unfinalized day → revert.
            let unfinalized = IOracle::getUtcDayVwapCall {
                base: "COEN".into(),
                quote: "0xUSD".into(),
                utcDay: 20260625u32,
            }
            .abi_encode();
            assert!(crate::precompile::dispatch(
                storage.clone(),
                &unfinalized,
                Address::ZERO,
                U256::ZERO
            )
            .is_err());
        });
    }

    #[test]
    fn test_lifecycle_finalizes_closed_utc_day() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            oracle.config_is_initialized.write(true).unwrap();
            oracle.config_vote_period.write(2).unwrap();
            let coen = oracle.register_pair("COEN", "0xUSD").unwrap();

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
            <crate::hooks::OracleLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();

            assert_eq!(oracle.utc_day_vwap_last_finalized.read().unwrap(), day_d);
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(day_d, coen).unwrap(),
                Some(U256::from(170u64))
            );
            // The in-progress current day is not finalized.
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(day_d1, coen).unwrap(),
                None
            );

            // Idempotent: a later block on the same UTC day neither advances the
            // watermark nor re-finalizes.
            let ctx2 = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(13, d1_start + 50, 1),
                storage.clone(),
            );
            <crate::hooks::OracleLifecycle as BlockLifecycle>::begin_block(&ctx2).unwrap();
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
            <crate::hooks::OracleLifecycle as BlockLifecycle>::begin_block(&ctx3).unwrap();
            assert_eq!(oracle.utc_day_vwap_last_finalized.read().unwrap(), day_d1);
            assert_eq!(
                oracle.get_utc_day_vwap_for_pair_id(day_d1, coen).unwrap(),
                Some(U256::from(190u64))
            );
        });
    }

    #[test]
    fn gas_cost_vwap_50h_window_with_varying_snapshot_counts() {
        // Measures gas cost of calculate_vwap for a 50-hour window
        // with increasing snapshot counts (simulating real testnet load).
        // Each snapshot has 1 pair. vote_period=2 blocks, ~1s blocks.
        // 50h = 180,000s → ~90,000 snapshots at 1 per 2s.
        // We test smaller counts to show the linear growth curve.
        let counts = [100, 500, 1_000, 5_000, 10_000];
        let window_seconds: u64 = 50 * 3600; // 50 hours

        for &n in &counts {
            let mut storage = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut storage, |handle| {
                let mut oracle = OracleContract::new(handle.clone());
                oracle.register_pair("COEN", "0xUSD").unwrap();

                let start_ts: u64 = 1_000_000;
                let interval = window_seconds / n;
                for i in 0..n {
                    let ts = start_ts + i * interval;
                    let price = U256::from(100 + (i % 10)) * SCALE_1E18;
                    let volume = U256::from(1000u64) * SCALE_1E18;
                    oracle.write_snapshot(ts, &[(1u32, price, volume)]).unwrap();
                }

                let _vwap = oracle
                    .calculate_vwap(1, start_ts, start_ts + window_seconds)
                    .unwrap();

                // Gas estimation: each snapshot in range costs ~5 sloads
                // (timestamp + pair_count + pair_id + rate + volume).
                // calculate_vwaps calls this for each vote_target pair.
                // With 7 pairs: 7 × n × 5 sloads × 100 gas = 3500 × n gas.
                let estimated_gas_per_pair = n * 5 * 100; // 1 pair
                let estimated_gas_7_pairs = estimated_gas_per_pair * 7;

                eprintln!(
                    "VWAP cost: snapshots={n}, window=50h, estimated_gas_1pair={estimated_gas_per_pair}, estimated_gas_7pairs={estimated_gas_7_pairs}, fits_100M={}",
                    estimated_gas_7_pairs < 100_000_000
                );

                assert!(
                    estimated_gas_7_pairs < 100_000_000,
                    "VWAP with {n} snapshots × 7 pairs costs ~{estimated_gas_7_pairs} gas — exceeds 100M"
                );
            });
        }
    }

    // === Reference currencies tests ===

    #[test]
    fn test_genesis_seeds_reference_currencies_with_usd() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(
                &mut oracle,
                &crate::logic::OracleGenesisConfig::default_config(),
            )
            .unwrap();

            assert_eq!(oracle.reference_currencies.len().unwrap(), 1);
            assert_eq!(oracle.reference_currencies.get(0).unwrap(), Some(840));
        });
    }

    #[test]
    fn test_genesis_seeds_custom_reference_currencies() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let config = crate::logic::OracleGenesisConfig {
                reference_currencies: vec![840, 978, 392],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            assert_eq!(oracle.reference_currencies.len().unwrap(), 3);
            assert_eq!(oracle.reference_currencies.get(0).unwrap(), Some(840));
            assert_eq!(oracle.reference_currencies.get(1).unwrap(), Some(978));
            assert_eq!(oracle.reference_currencies.get(2).unwrap(), Some(392));
        });
    }

    #[test]
    fn test_init_from_genesis_rejects_zero_reference_iso_code() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let config = crate::logic::OracleGenesisConfig {
                reference_currencies: vec![0],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            let err = crate::logic::init_from_genesis(&mut oracle, &config).unwrap_err();
            let msg = format!("{err:?}");
            assert!(
                msg.contains("reference iso_code must be non-zero"),
                "unexpected error: {msg}"
            );
        });
    }

    #[test]
    fn test_init_from_genesis_rejects_duplicate_reference_iso_code() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let config = crate::logic::OracleGenesisConfig {
                reference_currencies: vec![840, 840],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            let err = crate::logic::init_from_genesis(&mut oracle, &config).unwrap_err();
            let msg = format!("{err:?}");
            assert!(
                msg.contains("duplicate reference iso_code"),
                "unexpected error: {msg}"
            );
        });
    }

    #[test]
    fn test_export_genesis_round_trips_reference_currencies() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let config = crate::logic::OracleGenesisConfig {
                reference_currencies: vec![840, 978],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();

            let exported = crate::logic::export_genesis(&oracle, &[]).unwrap();
            assert_eq!(exported.reference_currencies, vec![840, 978]);
        });
    }

    #[test]
    fn test_check_reference_currency_ok_for_seeded_code() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(
                &mut oracle,
                &crate::logic::OracleGenesisConfig::default_config(),
            )
            .unwrap();
            drop(oracle);

            let ctx = BlockRuntimeContext::new(
                BlockContext::new(1, 1, 1, Address::ZERO, Vec::new()),
                storage,
            );
            crate::api::check_reference_currency(&ctx, 840).unwrap();
        });
    }

    #[test]
    fn test_check_reference_currency_err_for_missing_code() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            crate::logic::init_from_genesis(
                &mut oracle,
                &crate::logic::OracleGenesisConfig::default_config(),
            )
            .unwrap();
            drop(oracle);

            let ctx = BlockRuntimeContext::new(
                BlockContext::new(1, 1, 1, Address::ZERO, Vec::new()),
                storage,
            );
            let err = crate::api::check_reference_currency(&ctx, 978).unwrap_err();
            let msg = format!("{err:?}");
            assert!(
                msg.contains("not a registered reference currency"),
                "unexpected error: {msg}"
            );
        });
    }

    #[test]
    fn test_get_reference_currencies_precompile_returns_seeded_list() {
        with_storage(|storage| {
            let mut oracle = OracleContract::new(storage.clone());
            let config = crate::logic::OracleGenesisConfig {
                reference_currencies: vec![840, 978],
                ..crate::logic::OracleGenesisConfig::default_config()
            };
            crate::logic::init_from_genesis(&mut oracle, &config).unwrap();
            drop(oracle);

            use crate::precompile::IOracle;
            use alloy_sol_types::SolCall;

            let call = IOracle::getReferenceCurrenciesCall {}.abi_encode();
            let decoded = IOracle::getReferenceCurrenciesCall::abi_decode_returns(
                &crate::precompile::dispatch(storage, &call, Address::ZERO, U256::ZERO).unwrap(),
            )
            .unwrap();
            assert_eq!(decoded, vec![840u16, 978u16]);
        });
    }

    /// Probes the macro-assigned slot for `reference_currencies` so that
    /// `scripts/seed_genesis.py` can mirror the layout. The StorageVec stores
    /// its length at the base slot; we push two values and then linearly scan
    /// slots 0..128 looking for the length cell (== 2) to recover the slot.
    #[test]
    fn test_reference_currencies_slot_parity() {
        use outbe_primitives::addresses::ORACLE_ADDRESS;

        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            oracle.reference_currencies.push(840).unwrap();
            oracle.reference_currencies.push(978).unwrap();

            // Linear scan to find the slot whose word equals 2 (the length).
            let mut found: Option<u64> = None;
            for slot in 0u64..128 {
                let word = storage.sload(ORACLE_ADDRESS, U256::from(slot)).unwrap();
                if word == U256::from(2u64) {
                    found = Some(slot);
                    break;
                }
            }
            let slot = found.expect("could not locate reference_currencies length slot");

            println!("reference_currencies base slot = {slot}");

            // Verify the data lives at keccak256(slot) + 0 / + 1.
            use alloy_primitives::keccak256;
            let data_start = U256::from_be_bytes(keccak256(U256::from(slot).to_be_bytes::<32>()).0);
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, data_start).unwrap(),
                U256::from(840u64),
                "data[0] mismatch at slot {slot}"
            );
            assert_eq!(
                storage
                    .sload(ORACLE_ADDRESS, data_start + U256::from(1u64))
                    .unwrap(),
                U256::from(978u64),
                "data[1] mismatch at slot {slot}"
            );

            // Hard-coded slot used by scripts/seed_genesis.py; keep in sync.
            assert_eq!(
                slot, 55,
                "macro-assigned reference_currencies slot changed; update scripts/seed_genesis.py"
            );
        });
    }

    /// Parity guard for the `settlement_iso_to_pair` base slot used by
    /// `scripts/seed_genesis.py` (slot 42). Writes a distinctive marker, then
    /// scans base slots 0..128 to recover the macro-assigned slot via the
    /// known `keccak256(left_pad(key, 32) || be(base, 32))` derivation.
    #[test]
    fn test_settlement_iso_to_pair_slot_parity() {
        use alloy_primitives::{keccak256, B256};
        use outbe_primitives::addresses::ORACLE_ADDRESS;

        with_storage(|storage| {
            let oracle = OracleContract::new(storage.clone());
            let iso: u16 = 840;
            let marker = B256::repeat_byte(0xAB);
            oracle.settlement_iso_to_pair.write(&iso, marker).unwrap();

            for base in 0u64..128 {
                let mut buf = [0u8; 64];
                buf[30..32].copy_from_slice(&iso.to_be_bytes());
                buf[32..64].copy_from_slice(&U256::from(base).to_be_bytes::<32>());
                let slot = U256::from_be_bytes(keccak256(buf).0);
                let word = storage.sload(ORACLE_ADDRESS, slot).unwrap();
                if word == U256::from_be_bytes(marker.0) {
                    assert_eq!(
                        base, 42,
                        "macro-assigned settlement_iso_to_pair slot changed; \
                         update scripts/seed_genesis.py"
                    );
                    return;
                }
            }
            panic!("could not locate settlement_iso_to_pair base slot in 0..128");
        });
    }
}
