//! State-level tests: pair registry, orientation and exchange rates.

use alloy_primitives::{Address, U256};

use super::common::*;

#[test]
fn register_pair_assigns_sequential_ids_and_marks_vote_targets() {
    with_bare_oracle(|_storage, oracle| {
        // Register first pair
        assert_eq!(
            oracle
                .register_pair(AddressPair::from_addresses(COEN, USDT))
                .unwrap(),
            1
        );

        // Register second pair
        assert_eq!(
            oracle
                .register_pair(AddressPair::from_addresses(ETH, USDT))
                .unwrap(),
            2
        );

        // Verify lookup
        assert_eq!(oracle.pair_index_of(pair_key(COEN, USDT)).unwrap(), 1);
        assert_eq!(oracle.pair_index_of(pair_key(ETH, USDT)).unwrap(), 2);
        assert_eq!(oracle.pair_index_of(pair_key(BTC, USDT)).unwrap(), 0); // not registered

        // Verify vote targets
        assert!(oracle.is_vote_target(COEN, USDT).unwrap());
        assert!(oracle.is_vote_target(ETH, USDT).unwrap());
        assert!(!oracle.is_vote_target(BTC, USDT).unwrap());

        // Duplicate registration fails
        assert!(oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .is_err());

        // Pair count
        assert_eq!(oracle.pair_count.read().unwrap(), 2);
        assert_eq!(oracle.pair_at(1).unwrap(), pair_key(COEN, USDT));
        assert_eq!(oracle.pair_at(2).unwrap(), pair_key(ETH, USDT));
    });
}

#[test]
fn register_pair_rejects_the_inverse_of_a_registered_pair() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        // The key is order-independent, so the inverse is the same pair.
        assert!(oracle
            .register_pair(AddressPair::from_addresses(USDT, COEN))
            .is_err());
    });
}

#[test]
fn register_pair_rejects_an_asset_paired_with_itself() {
    with_bare_oracle(|_storage, oracle| {
        assert!(oracle
            .register_pair(AddressPair::from_addresses(USDT, USDT))
            .is_err());
        assert!(oracle
            .register_pair(AddressPair::from_addresses(COEN, COEN))
            .is_err());
    });
}

#[test]
fn register_pair_preserves_a_generic_market_orientation() {
    with_bare_oracle(|_storage, oracle| {
        // The ISO address sorts below every token stand-in. So this proves that the
        // registry value is the configured orientation, not the sorted storage-key
        // orientation.
        let backwards = AddressPair::from_addresses(ETH, usd());
        assert!(
            !backwards.is_canonical(),
            "fixture is not a backwards quote"
        );

        assert_eq!(oracle.register_pair(backwards).unwrap(), 1);
        assert_eq!(oracle.pair_at(1).unwrap(), backwards);
        assert_eq!(oracle.require_pair(backwards).unwrap(), backwards);
        assert!(oracle.require_pair(backwards.to_canonical()).is_err());
    });
}

#[test]
fn register_pair_rejects_iso_to_coen_but_accepts_coen_to_iso() {
    with_bare_oracle(|_storage, oracle| {
        let reverse = AddressPair::from_addresses(usd(), COEN);

        assert!(oracle.register_pair(reverse).is_err());
        assert_eq!(oracle.pair_count.read().unwrap(), 0);

        let forward = AddressPair::new_coen_to(840);
        assert_eq!(oracle.register_pair(forward).unwrap(), 1);
        assert_eq!(oracle.pair_at(1).unwrap(), forward);
    });
}

#[test]
fn reciprocal_read_is_relative_to_the_registered_generic_orientation() {
    with_bare_oracle(|_storage, oracle| {
        let registered = AddressPair::from_addresses(ETH, usd());
        let rate = fixed18(4);
        oracle.register_pair(registered).unwrap();
        oracle
            .set_exchange_rate(Address::ZERO, registered, rate, 7, 11)
            .unwrap();

        assert_eq!(oracle.get_exchange_rate(ETH, usd()).unwrap(), rate);
        assert_eq!(
            oracle.get_exchange_rate(usd(), ETH).unwrap(),
            fixed18(1) / U256::from(4u64)
        );
        assert_eq!(
            oracle.get_exchange_rate_data(usd(), ETH).unwrap(),
            (fixed18(1) / U256::from(4u64), 7, 11)
        );
    });
}

#[test]
fn require_pair_rejects_a_pair_quoted_in_the_wrong_direction() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        assert_eq!(
            oracle.require_pair_from(COEN, USDT).unwrap(),
            pair_key(COEN, USDT)
        );

        // Reads whose value has no direction of its own - a VWAP, an S-curve
        // peak - cannot answer a backwards quote, so they refuse it. Only the
        // spot rate has a reciprocal.
        let err = oracle.require_pair_from(USDT, COEN).unwrap_err();
        assert!(
            format!("{err:?}").contains("registered orientation"),
            "expected a registered-orientation revert, got {err:?}"
        );
        assert!(oracle.get_exchange_rate(USDT, COEN).is_ok());
    });
}

/// One unordered market key identifies one registration, while the registry
/// entry preserves exactly the configured orientation.
#[test]
fn one_market_is_one_registration_reachable_from_either_quote() {
    with_bare_oracle(|_storage, oracle| {
        let registered = AddressPair::from_addresses(usd(), ETH);
        assert_eq!(oracle.register_pair(registered).unwrap(), 1);

        // Order-independent: either direction finds the same registration.
        assert_eq!(oracle.pair_index_of(registered).unwrap(), 1);
        assert_eq!(
            oracle
                .pair_index_of(AddressPair::from_addresses(ETH, usd()))
                .unwrap(),
            1
        );

        // Registering the market backwards is a duplicate, not a second market.
        assert!(oracle
            .register_pair(AddressPair::from_addresses(ETH, usd()))
            .is_err());
        assert_eq!(oracle.pair_count.read().unwrap(), 1);

        let entry = oracle.pair_at(1).unwrap();
        assert_eq!(entry, registered);
        assert!(entry.is_canonical());
    });
}

#[test]
fn every_pair_read_agrees_on_the_market_whichever_way_it_is_quoted() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        let rate = fixed18(4);
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, USDT),
                rate,
                1,
                1,
            )
            .unwrap();

        assert!(oracle.is_vote_target(COEN, USDT).unwrap());
        assert_eq!(oracle.get_exchange_rate(COEN, USDT).unwrap(), rate);

        // Backwards: both answer for the same market, the rate as a reciprocal.
        // Disagreeing here would let a caller act on a rate it cannot fetch.
        assert!(oracle.is_vote_target(USDT, COEN).unwrap());
        assert_eq!(
            oracle.get_exchange_rate(USDT, COEN).unwrap(),
            fixed18(1) / U256::from(4u64)
        );

        // Writes stay in registered orientation: a backwards quote has no
        // direction-free reading on the way in.
        assert!(oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(USDT, COEN),
                U256::from(9u64),
                1,
                1
            )
            .is_err());
    });
}

#[test]
fn a_backwards_quote_prices_at_the_reciprocal() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        // 2.5 COEN per USDT.
        let rate = U256::from(2_500_000_000_000_000_000u128);
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, USDT),
                rate,
                42,
                86_400,
            )
            .unwrap();

        let (forward, fwd_block, fwd_ts) = oracle.get_exchange_rate_data(COEN, USDT).unwrap();
        let (backward, bwd_block, bwd_ts) = oracle.get_exchange_rate_data(USDT, COEN).unwrap();

        assert_eq!(forward, rate);
        assert_eq!(backward, U256::from(400_000_000_000_000_000u128));
        // The observation is one event. Only its quoting differs.
        assert_eq!((fwd_block, fwd_ts), (42, 86_400));
        assert_eq!((bwd_block, bwd_ts), (42, 86_400));
    });
}

#[test]
fn every_coen_iso_backwards_quote_uses_the_six_decimal_reciprocal() {
    with_bare_oracle(|_storage, oracle| {
        for iso in [840, 978] {
            let quote: Address = AssetType::IsoCurrency(iso).into();
            let pair = AddressPair::new_coen_to(iso);
            oracle.register_pair(pair).unwrap();
            oracle
                .set_exchange_rate(Address::ZERO, pair, U256::from(2_500_000u64), 42, 86_400)
                .unwrap();

            assert_eq!(
                oracle.get_exchange_rate(quote, COEN).unwrap(),
                U256::from(400_000u64)
            );
        }
    });
}

#[test]
fn an_unpublished_rate_reads_as_zero_from_either_side() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        // No reciprocal exists for zero; inverting must not divide by it.
        assert_eq!(oracle.get_exchange_rate(COEN, USDT).unwrap(), U256::ZERO);
        assert_eq!(oracle.get_exchange_rate(USDT, COEN).unwrap(), U256::ZERO);
    });
}

#[test]
fn a_rate_read_still_requires_a_registered_market() {
    with_bare_oracle(|_storage, oracle| {
        assert!(oracle.get_exchange_rate(COEN, USDT).is_err());
        assert!(oracle.get_exchange_rate(USDT, COEN).is_err());
        assert!(!oracle.is_vote_target(COEN, USDT).unwrap());
    });
}

#[test]
fn require_pair_at_rejects_an_index_outside_the_registry() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        assert_eq!(oracle.require_pair_at(1).unwrap(), pair_key(COEN, USDT));
        // Index 0 and one past the end both read back as the zero pair, which is
        // a plausible-looking COEN/COEN registration rather than an obvious miss.
        assert!(oracle.require_pair_at(0).is_err());
        assert!(oracle.require_pair_at(2).is_err());
    });
}

#[test]
fn a_pair_is_deterministic_direction_sensitive_and_distinct_per_market() {
    use outbe_primitives::storage::types::StorageKey;

    assert_eq!(pair_key(COEN, USDT), pair_key(COEN, USDT));
    assert_ne!(pair_key(COEN, USDT), pair_key(ETH, USDT));

    // The value keeps the quote direction. Only the storage key drops it.
    assert_ne!(pair_key(COEN, USDT), pair_key(USDT, COEN));
    assert!(pair_key(COEN, USDT).same_market(&pair_key(USDT, COEN)));
    assert_eq!(
        pair_key(COEN, USDT).key_bytes(),
        pair_key(USDT, COEN).key_bytes()
    );
    assert!(!pair_key(COEN, USDT).same_market(&pair_key(ETH, USDT)));
}

#[test]
fn set_exchange_rate_round_trips_rate_block_and_timestamp() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        // Set rate (system call)
        let rate = U256::from(1_500_000_000_000_000_000u128); // 1.5
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, USDT),
                rate,
                100,
                1200,
            )
            .unwrap();

        // Read back
        assert_eq!(
            oracle.get_exchange_rate_data(COEN, USDT).unwrap(),
            (rate, 100, 1200)
        );
    });
}

#[test]
fn set_exchange_rate_rejects_a_non_system_caller() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        let caller = Address::new([1u8; 20]);
        let result = oracle.set_exchange_rate(
            caller,
            AddressPair::from_addresses(COEN, USDT),
            U256::from(1u64),
            0,
            0,
        );
        assert!(result.is_err());
    });
}

#[test]
fn get_exchange_rate_reverts_for_an_unregistered_pair() {
    with_bare_oracle(|_storage, oracle| {
        assert!(oracle.get_exchange_rate(BTC, USDT).is_err());
    });
}

/// The three rate columns key on the registry index, so a price read is
/// `pair_to_index` and then slot 12. This test pins the raw slots that
/// `scripts/seed_genesis.py` writes. It also asserts that nothing lands at the
/// pair-derived slot they used to use. Otherwise a schema key-type revert would
/// pass every behavioural test above while silently orphaning every seeded rate.
#[test]
fn the_rate_columns_are_keyed_by_the_registry_index() {
    use outbe_primitives::addresses::ORACLE_ADDRESS;
    use outbe_primitives::storage::types::StorageKey;

    with_bare_oracle(|storage, oracle| {
        let index = oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();
        let rate = U256::from(1_500_000_000_000_000_000u128);
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, USDT),
                rate,
                42,
                86_400,
            )
            .unwrap();

        let at = |slot: U256| storage.sload(ORACLE_ADDRESS, slot).unwrap();
        for (base, expected, field) in [
            (12u64, rate, "exchange_rate"),
            (13, U256::from(42u64), "exchange_rate_block"),
            (14, U256::from(86_400u64), "exchange_rate_timestamp"),
        ] {
            let base = U256::from(base);
            assert_eq!(
                at(index.mapping_slot(base)),
                expected,
                "{field} is not keyed by the registry index at base slot {base}; \
                 scripts/seed_genesis.py hardcodes it"
            );
            assert_eq!(
                at(pair_key(COEN, USDT).mapping_slot(base)),
                U256::ZERO,
                "{field} still writes the pair-derived slot"
            );
        }
    });
}

/// Deactivated pairs lose their rate, and an active neighbour registered after
/// them keeps its own. The clear walks the registry by index, so an off-by-one
/// would wipe the wrong column.
#[test]
fn remove_excess_feeds_clears_only_the_deactivated_pairs_rate() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, USDT),
                U256::from(7u64),
                10,
                120,
            )
            .unwrap();
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(ETH, USDT),
                U256::from(9u64),
                20,
                240,
            )
            .unwrap();

        oracle
            .deactivate_vote_target(Address::ZERO, COEN, USDT)
            .unwrap();
        oracle.remove_excess_feeds().unwrap();

        assert_eq!(
            oracle.get_exchange_rate_data(COEN, USDT).unwrap(),
            (U256::ZERO, 0, 0)
        );
        assert_eq!(
            oracle.get_exchange_rate_data(ETH, USDT).unwrap(),
            (U256::from(9u64), 20, 240)
        );
    });
}

#[test]
fn config_slots_round_trip_every_genesis_parameter() {
    with_bare_oracle(|_storage, oracle| {
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
    with_bare_oracle(|_storage, oracle| {
        record_outcomes(
            oracle,
            &FIRST_VOTER,
            &[
                Penalty::Success,
                Penalty::Success,
                Penalty::Miss,
                Penalty::Abstain,
            ],
        );

        assert_penalty_counters(
            oracle,
            &[
                (FIRST_VOTER, Penalty::Success, 2),
                (FIRST_VOTER, Penalty::Miss, 1),
                (FIRST_VOTER, Penalty::Abstain, 1),
            ],
        );

        oracle.reset_penalty_counter(&FIRST_VOTER).unwrap();
        assert_penalty_counters(
            oracle,
            &[
                (FIRST_VOTER, Penalty::Success, 0),
                (FIRST_VOTER, Penalty::Miss, 0),
                (FIRST_VOTER, Penalty::Abstain, 0),
            ],
        );
    });
}
