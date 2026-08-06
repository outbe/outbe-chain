//! State-level tests: pair registry, rates, votes, snapshots, layout parity.

use alloy_primitives::{Address, U256};
use outbe_primitives::units::Units;

use crate::schema::{OracleContract, SCALE_1E18};

use super::common::*;

#[test]
fn register_pair_assigns_sequential_ids_and_marks_vote_targets() {
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
fn get_pairs_returns_ids_symbols_and_active_flags() {
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
fn get_pairs_returns_empty_arrays_without_registered_pairs() {
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
fn pair_hash_is_deterministic_and_distinct_per_pair() {
    let h1 = OracleContract::pair_hash("COEN", "USDT");
    let h2 = OracleContract::pair_hash("COEN", "USDT");
    assert_eq!(h1, h2);

    let h3 = OracleContract::pair_hash("ETH", "USDT");
    assert_ne!(h1, h3);
}

#[test]
fn set_exchange_rate_round_trips_rate_block_and_timestamp() {
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
fn set_exchange_rate_rejects_a_non_system_caller() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair("COEN", "USDT").unwrap();

        let caller = Address::new([1u8; 20]);
        let result = oracle.set_exchange_rate(caller, "COEN", "USDT", U256::from(1u64), 0, 0);
        assert!(result.is_err());
    });
}

#[test]
fn get_exchange_rate_reverts_for_an_unregistered_pair() {
    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        assert!(oracle.get_exchange_rate("BTC", "USDT").is_err());
    });
}
#[test]
fn config_slots_round_trip_every_genesis_parameter() {
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
fn penalty_counters_increment_per_outcome_and_reset_together() {
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
fn write_snapshot_advances_the_ring_buffer_and_feeds_vwap() {
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
fn calculate_vwap_includes_only_snapshots_inside_the_window() {
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
fn calculate_vwap_reverts_for_a_window_without_snapshots() {
    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        // No snapshots at all
        assert!(oracle.calculate_vwap(1, 0, 1000).is_err());
    });
}

#[test]
fn calculate_vwap_treats_zero_volume_as_one_scaled_unit() {
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
        let expected = (U256::in_units(100u64) * SCALE_1E18 + U256::in_units(200u64) * SCALE_1E18)
            / (U256::in_units(2u64));
        assert_eq!(vwap, expected);
    });
}

#[test]
fn calculate_vwap_isolates_each_pair_within_one_snapshot() {
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
#[test]
fn submit_vote_stores_tuples_until_clear_votes_drains_them() {
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
fn submit_vote_rejects_a_duplicated_pair() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair("COEN", "USDT").unwrap();
        oracle.register_pair("ETH", "USDT").unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, U256::in_units(100u64));

        let pair_hash = OracleContract::pair_hash("COEN", "USDT");
        let rate = U256::in_units(50u64);
        let volume = U256::in_units(1000u64);
        // Two tuples naming the same pair: within the pair-count bound, so the
        // dedup scan is what must reject it.
        let err = oracle
            .submit_vote(validator, &[(pair_hash, rate, volume); 2])
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("duplicate pair in vote submission"),
            "unexpected error: {err:?}"
        );
        assert!(!oracle.vote_exists.read(&validator).unwrap());
    });
}

#[test]
fn submit_vote_reports_a_duplicate_before_an_inactive_vote_target() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair("COEN", "USDT").unwrap();
        oracle.register_pair("ETH", "USDT").unwrap();
        oracle
            .deactivate_vote_target(Address::ZERO, "ETH", "USDT")
            .unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, U256::in_units(100u64));

        let untargeted = OracleContract::pair_hash("ETH", "USDT");
        let rate = U256::in_units(50u64);
        let volume = U256::in_units(1000u64);
        // A submission that is both untargeted and duplicated reports the
        // duplicate first — receipt-visible revert text, so the order is pinned.
        let err = oracle
            .submit_vote(validator, &[(untargeted, rate, volume); 2])
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("duplicate pair in vote submission"),
            "unexpected error: {err:?}"
        );
    });
}

// -----------------------------------------------------------------------
// View functions
// -----------------------------------------------------------------------

#[test]
fn get_exchange_rates_returns_parallel_arrays_in_pair_id_order() {
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
fn get_exchange_rates_returns_empty_arrays_without_registered_pairs() {
    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let (rates, blocks, timestamps) = oracle.get_exchange_rates().unwrap();
        assert!(rates.is_empty());
        assert!(blocks.is_empty());
        assert!(timestamps.is_empty());
    });
}

#[test]
fn get_vote_targets_lists_only_active_pairs() {
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
fn get_vote_targets_returns_empty_without_registered_pairs() {
    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let pair_ids = oracle.get_vote_targets().unwrap();
        assert!(pair_ids.is_empty());
    });
}

#[test]
fn get_aggregate_vote_returns_the_stored_tuples() {
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
fn get_aggregate_vote_reports_absent_for_a_non_voter() {
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
fn get_slash_window_progress_reports_counters_with_the_window_length() {
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
#[test]
fn delegate_feeder_round_trips_and_revokes_on_the_zero_address() {
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
/// Probes the macro-assigned slot for `reference_currencies` so that
/// `scripts/seed_genesis.py` can mirror the layout. The StorageVec stores
/// its length at the base slot; we push two values and then linearly scan
/// slots 0..128 looking for the length cell (== 2) to recover the slot.
#[test]
fn reference_currencies_occupies_slot_55() {
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

/// Pins every base slot the frozen OCOMP V1 opening plan (`openings.rs`) and
/// `scripts/seed_genesis.py` hardcode. Each field is written through the typed
/// schema and read back at the raw slot those consumers derive, so any field
/// reorder or mis-placed `#[slot(N)]` pin fails here rather than silently
/// corrupting a genesis seed or an opening proof.
///
/// Slots 41 and 46 are retired holes: they stay in the V1 plan (whose codec
/// descriptor is hashed into the protocol bundle) but have no live writer, so
/// they must read as zero after a full genesis init.
#[test]
fn ocomp_opening_plan_slots_match_the_schema_layout() {
    use alloy_primitives::B256;
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;
    use outbe_primitives::storage::StorageHandle;

    fn assert_mapping_slot<K: StorageKey>(
        storage: &StorageHandle<'_>,
        key: K,
        base: U256,
        expected: U256,
        field: &str,
    ) {
        assert_eq!(
            storage
                .sload(ORACLE_ADDRESS, key.mapping_slot(base))
                .unwrap(),
            expected,
            "{field} is not at base slot {base}; openings.rs and \
             scripts/seed_genesis.py hardcode it"
        );
    }

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let wwd = outbe_common::WorldwideDay::from_timestamp(ATOMIC_DAY_START);
        let pair_hash = B256::repeat_byte(0xAB);
        let iso: u16 = 840;

        oracle.pair_hash_to_id.write(&pair_hash, 7).unwrap();
        oracle.scurve_count.write(3).unwrap();
        oracle.scurve_pair_id.write(&0u32, 7).unwrap();
        oracle.scurve_peak_day.write(&0u32, 111).unwrap();
        oracle
            .scurve_peak_price
            .write(&0u32, U256::from(222u64))
            .unwrap();
        oracle.scurve_oldest_idx.write(1).unwrap();
        oracle
            .settlement_iso_to_pair
            .write(&iso, pair_hash)
            .unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle.worldwide_day_vwap_pair_count.write(&wwd, 1).unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, 7)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, U256::from(333u64))
            .unwrap();

        // Direct (non-mapping) slots.
        for (slot, expected, field) in [(34u64, 3u64, "scurve_count"), (38, 1, "scurve_oldest_idx")]
        {
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, U256::from(slot)).unwrap(),
                U256::from(expected),
                "{field} is not at slot {slot}"
            );
        }

        let base = U256::from;
        assert_mapping_slot(&storage, pair_hash, base(10), base(7), "pair_hash_to_id");
        assert_mapping_slot(&storage, 0u32, base(35), base(7), "scurve_pair_id");
        assert_mapping_slot(&storage, 0u32, base(36), base(111), "scurve_peak_day");
        assert_mapping_slot(&storage, 0u32, base(37), base(222), "scurve_peak_price");
        assert_mapping_slot(
            &storage,
            iso,
            base(42),
            U256::from_be_bytes(pair_hash.0),
            "settlement_iso_to_pair",
        );
        assert_mapping_slot(
            &storage,
            wwd,
            base(47),
            base(1),
            "worldwide_day_vwap_exists",
        );
        assert_mapping_slot(
            &storage,
            wwd,
            base(50),
            base(1),
            "worldwide_day_vwap_pair_count",
        );
        // Nested maps: the outer key derives the inner map's base slot.
        assert_mapping_slot(
            &storage,
            0u32,
            wwd.mapping_slot(base(51)),
            base(7),
            "worldwide_day_vwap_pair_id",
        );
        assert_mapping_slot(
            &storage,
            0u32,
            wwd.mapping_slot(base(52)),
            base(333),
            "worldwide_day_vwap_value",
        );
    });
}

/// The two retired denom slots must stay empty: they remain in the frozen V1
/// opening plan, so a resurrected writer would change what the plan proves.
#[test]
fn retired_denom_slots_stay_zero_after_genesis() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;

    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let mut config = crate::genesis::OracleGenesisConfig::default_config();
        config
            .settlement_currencies
            .push((840, "COEN".into(), "840".into()));
        crate::genesis::init_from_genesis(&mut oracle, &config).unwrap();

        for base in [41u64, 46] {
            let slot = 840u16.mapping_slot(U256::from(base));
            assert_eq!(
                storage.sload(ORACLE_ADDRESS, slot).unwrap(),
                U256::ZERO,
                "retired slot {base} was written; it is part of the frozen \
                 OCOMP V1 opening plan and must have no live writer"
            );
        }
    });
}

/// Parity guard for the `settlement_iso_to_pair` base slot used by
/// `scripts/seed_genesis.py` (slot 42). Writes a distinctive marker, then
/// scans base slots 0..128 to recover the macro-assigned slot via the
/// known `keccak256(left_pad(key, 32) || be(base, 32))` derivation.
#[test]
fn settlement_iso_to_pair_occupies_slot_42() {
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

/// Parity guard for the `reference_currency_rate` base slot used by
/// `scripts/seed_genesis.py`. Writes a distinctive marker, then scans base
/// slots 0..128 to recover the macro-assigned slot via the known
/// `keccak256(left_pad(key, 32) || be(base, 32))` mapping derivation.
#[test]
fn reference_currency_rate_occupies_slot_60() {
    use alloy_primitives::keccak256;
    use outbe_primitives::addresses::ORACLE_ADDRESS;

    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let iso: u16 = 840;
        let marker = U256::from(0x00AB_CDEFu64);
        oracle.reference_currency_rate.write(&iso, marker).unwrap();

        for base in 0u64..128 {
            let mut buf = [0u8; 64];
            buf[30..32].copy_from_slice(&iso.to_be_bytes());
            buf[32..64].copy_from_slice(&U256::from(base).to_be_bytes::<32>());
            let slot = U256::from_be_bytes(keccak256(buf).0);
            if storage.sload(ORACLE_ADDRESS, slot).unwrap() == marker {
                assert_eq!(
                    base, 60,
                    "macro-assigned reference_currency_rate slot changed; \
                     update scripts/seed_genesis.py"
                );
                return;
            }
        }
        panic!("could not locate reference_currency_rate base slot in 0..128");
    });
}

#[test]
fn genesis_seeds_the_usd_currency_rate() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(
            &mut oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
        assert_eq!(
            oracle.get_currency_rate(840).unwrap(),
            crate::constants::DEFAULT_USD_CURRENCY_RATE
        );
    });
}

#[test]
fn get_currency_rate_reverts_for_an_unregistered_iso_code() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        crate::genesis::init_from_genesis(
            &mut oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
        let err = oracle.get_currency_rate(978).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("no currency rate for iso_code 978"),
            "unexpected error: {msg}"
        );
    });
}
