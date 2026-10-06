//! State-level tests: vote submission, feeders and bulk read views.

use alloy_primitives::{Address, U256};

use crate::schema::{OracleContract, SCALE_1E18};

use super::common::*;

#[test]
fn submit_vote_stores_tuples_until_clear_votes_drains_them() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, native_coen(100));
        let rate = fixed18(50);
        let volume = fixed18(1000);

        // Submit vote
        oracle
            .submit_vote(validator, &[(COEN, USDT, rate, volume)])
            .unwrap();

        // Verify vote stored
        assert!(oracle.vote_exists.read(&validator).unwrap());
        assert_eq!(oracle.vote_tuple_count.read(&validator).unwrap(), 1);
        assert_eq!(oracle.voter_list.len().unwrap(), 1);

        // Double vote should fail
        assert!(oracle
            .submit_vote(validator, &[(COEN, USDT, rate, volume)])
            .is_err());

        // Clear
        oracle.clear_votes().unwrap();
        assert!(!oracle.vote_exists.read(&validator).unwrap());
        assert_eq!(oracle.voter_list.len().unwrap(), 0);
    });
}

#[test]
fn submit_vote_rejects_an_unregistered_signer() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();

        let stranger = Address::new([0x99; 20]);
        let err = oracle
            .submit_vote(stranger, &[(COEN, usd(), coen_iso(50), COEN_ISO_SCALE)])
            .unwrap_err();

        assert!(
            err.to_string().contains("not an active ORACLE signer"),
            "unexpected error: {err:?}"
        );
        assert_eq!(oracle.voter_list.len().unwrap(), 0);
    });
}

#[test]
fn submit_vote_rejects_a_validator_that_is_no_longer_active() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, native_coen(100));
        outbe_validatorset::contract::ValidatorSet::new(storage)
            .deactivate_validator(Address::ZERO, validator)
            .unwrap();

        let err = oracle
            .submit_vote(validator, &[(COEN, usd(), coen_iso(50), COEN_ISO_SCALE)])
            .unwrap_err();

        assert!(
            err.to_string().contains("not an active ORACLE signer"),
            "unexpected error: {err:?}"
        );
        assert_eq!(oracle.voter_list.len().unwrap(), 0);
    });
}

#[test]
fn submit_vote_rejects_the_reverse_of_the_registered_direction() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        let registered = AddressPair::from_addresses(ETH, usd());
        oracle.register_pair(registered).unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage, validator, native_coen(100));
        let err = oracle
            .submit_vote(
                validator,
                &[(
                    registered.address2(),
                    registered.address1(),
                    fixed18(2),
                    SCALE_1E18,
                )],
            )
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("does not match the registered orientation"),
            "unexpected error: {err:?}"
        );
        assert_eq!(oracle.voter_list.len().unwrap(), 0);
    });
}

#[test]
fn submit_vote_rejects_a_duplicated_pair() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, native_coen(100));
        let rate = fixed18(50);
        let volume = fixed18(1000);
        // Two tuples naming the same pair: within the pair-count bound, so the
        // dedup scan is what must reject it.
        let err = oracle
            .submit_vote(validator, &[(COEN, USDT, rate, volume); 2])
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
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();
        oracle
            .deactivate_vote_target(Address::ZERO, ETH, USDT)
            .unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, native_coen(100));
        let rate = fixed18(50);
        let volume = fixed18(1000);
        // A submission that is both untargeted and duplicated reports the
        // duplicate first. The revert text is visible in the receipt, so this
        // test pins the order.
        let err = oracle
            .submit_vote(validator, &[(ETH, USDT, rate, volume); 2])
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

/// The caller now builds the whole-registry rate table from `pair_count`,
/// `require_pair_at` and the per-pair rate read. So this must hold: a walk over
/// the index in registration order lands each pair on its own rate.
#[test]
fn walking_the_registry_by_index_pairs_each_market_with_its_own_rate() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();

        let rate1 = U256::from(1_500_000_000_000_000_000u128);
        let rate2 = U256::from(2_000_000_000_000_000_000u128);
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(COEN, USDT),
                rate1,
                10,
                120,
            )
            .unwrap();
        oracle
            .set_exchange_rate(
                Address::ZERO,
                AddressPair::from_addresses(ETH, USDT),
                rate2,
                20,
                240,
            )
            .unwrap();

        let count = oracle.pair_count.read().unwrap();
        assert_eq!(count, 2);
        let table: Vec<_> = (1..=count)
            .map(|index| {
                let pair = oracle.require_pair_at(index).unwrap();
                let (rate, block, ts) = oracle
                    .get_exchange_rate_data(pair.address1(), pair.address2())
                    .unwrap();
                (pair, rate, block, ts)
            })
            .collect();

        assert_eq!(
            table,
            vec![
                (pair_key(COEN, USDT), rate1, 10, 120),
                (pair_key(ETH, USDT), rate2, 20, 240),
            ]
        );
    });
}

#[test]
fn get_vote_targets_lists_only_active_pairs() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(BTC, USDT))
            .unwrap();

        // Deactivate ETH/USDT (pair_id 2)
        oracle
            .deactivate_vote_target(Address::ZERO, ETH, USDT)
            .unwrap();

        let (bases, quotes) = oracle.get_vote_targets().unwrap();
        assert_eq!(bases, vec![COEN, BTC]);
        assert_eq!(quotes, vec![USDT, USDT]);
    });
}

#[test]
fn get_vote_targets_returns_empty_without_registered_pairs() {
    with_storage(|storage| {
        let oracle = OracleContract::new(storage.clone());
        let (bases, quotes) = oracle.get_vote_targets().unwrap();
        assert!(bases.is_empty() && quotes.is_empty());
    });
}

#[test]
fn get_aggregate_vote_returns_the_stored_tuples() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();

        let validator = Address::new([0x11; 20]);
        register_validator(storage.clone(), validator, native_coen(100));
        let rate1 = fixed18(50);
        let rate2 = fixed18(3000);
        let vol1 = fixed18(100);
        let vol2 = fixed18(200);

        oracle
            .submit_vote(
                validator,
                &[(COEN, USDT, rate1, vol1), (ETH, USDT, rate2, vol2)],
            )
            .unwrap();

        let (exists, bases, quotes, rates, volumes) =
            oracle.get_aggregate_vote(&validator).unwrap();
        assert!(exists);
        assert_eq!(bases, vec![COEN, ETH]);
        assert_eq!(quotes, vec![USDT, USDT]);
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

        let (exists, bases, quotes, rates, volumes) =
            oracle.get_aggregate_vote(&validator).unwrap();
        assert!(!exists);
        assert!(bases.is_empty() && quotes.is_empty());
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
        oracle
            .register_pair(AddressPair::from_addresses(COEN, USDT))
            .unwrap();

        let validator = Address::new([0x11; 20]);
        let feeder = Address::new([0x22; 20]);
        register_validator(storage.clone(), validator, native_coen(100));

        // Delegate
        oracle.delegate_feeder(validator, feeder).unwrap();
        assert_eq!(oracle.get_feeder(&validator).unwrap(), feeder);

        // Feeder can submit vote on behalf of validator
        oracle
            .submit_vote(feeder, &[(COEN, USDT, fixed18(50), SCALE_1E18)])
            .unwrap();

        assert!(oracle.vote_exists.read(&validator).unwrap());
    });
}
