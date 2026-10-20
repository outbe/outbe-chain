//! State-level tests: finalized trailing-window VWAP, policy identity and
//! closed-day history. Round coverage of the window lives in `coverage`.

use alloy_primitives::{Address, U256};

use crate::schema::OracleContract;

use super::common::*;

pub(super) fn default_snapshot_at(timestamp: u64) -> crate::window::VwapSnapshotId {
    crate::window::get_vwap_snapshot_id(timestamp, &crate::window::DEFAULT_VWAP_POLICY).unwrap()
}

#[test]
fn current_snapshot_dispatch_rejects_wide_timestamp_instead_of_panicking() {
    use alloy_sol_types::SolCall;
    use outbe_primitives::error::PrecompileError;

    with_storage_at(ATOMIC_DAY_START, |storage| {
        let call = crate::precompile::IOracle::getVwapSnapshotIdCall {}.abi_encode();
        assert!(
            crate::precompile::dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO).is_ok()
        );
        for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
            storage.set_block_timestamp(timestamp).unwrap();
            assert!(matches!(
                crate::precompile::dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO),
                Err(PrecompileError::Revert(message)) if message == "timestamp exceeds u64"
            ));
            assert!(matches!(
                crate::api::current_vwap_snapshot(storage.clone()),
                Err(PrecompileError::Revert(message)) if message == "timestamp exceeds u64"
            ));
        }
    });
}

#[test]
fn finalized_window_vwap_weights_whole_window_volume() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour + 37 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        oracle
            .write_snapshot(day + 3 * hour + 5, &[(pair, coen_iso(1), coen_iso(1))])
            .unwrap();
        oracle
            .write_snapshot(day + 7 * hour + 5, &[(pair, coen_iso(3), coen_iso(9))])
            .unwrap();

        let snapshot = default_snapshot_at(day + 10 * hour + 37 * 60);
        assert_eq!(
            oracle.finalized_window_vwap(pair, snapshot).unwrap(),
            Some(U256::from(2_800_000u64))
        );
    });
}

#[test]
fn finalized_window_vwap_is_half_open_and_counts_overlapping_hours_once() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 11 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        for (ts, price) in [
            (day + 2 * hour, 10),
            (day + 3 * hour + 60, 20),
            (day + 9 * hour + 60, 30),
            (day + 10 * hour, 40),
        ] {
            oracle
                .write_snapshot(ts, &[(pair, coen_iso(price), coen_iso(1))])
                .unwrap();
        }

        let at_ten = default_snapshot_at(day + 10 * hour);
        let at_eleven = default_snapshot_at(day + 11 * hour);
        assert_eq!(
            oracle.finalized_window_vwap(pair, at_ten).unwrap(),
            Some(coen_iso(20))
        );
        assert_eq!(
            oracle.finalized_window_vwap(pair, at_eleven).unwrap(),
            Some(coen_iso(30))
        );
    });
}

#[test]
fn finalized_window_vwap_rejects_an_open_window_and_reports_an_empty_one() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour + 59 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();

        let open = default_snapshot_at(day + 11 * hour);
        assert!(oracle.finalized_window_vwap(pair, open).is_err());
        let closed = default_snapshot_at(day + 10 * hour);
        assert_eq!(oracle.finalized_window_vwap(pair, closed).unwrap(), None);
        assert_eq!(
            crate::api::get_finalized_window_vwap(storage.clone(), 978, closed).unwrap(),
            None
        );
    });
}

#[test]
fn finalized_window_vwap_never_falls_back_to_an_earlier_window() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 11 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        oracle
            .write_snapshot(day + 2 * hour + 1_800, &[(pair, coen_iso(7), coen_iso(1))])
            .unwrap();

        assert_eq!(
            oracle
                .finalized_window_vwap(pair, default_snapshot_at(day + 10 * hour))
                .unwrap(),
            Some(coen_iso(7))
        );
        assert_eq!(
            oracle
                .finalized_window_vwap(pair, default_snapshot_at(day + 11 * hour))
                .unwrap(),
            None
        );
    });
}

#[test]
fn finalized_window_vwap_spans_midnight() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 3 * hour + 1_800, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        for (ts, price) in [
            (day - 6 * hour, 1_000),
            (day - 4 * hour, 10),
            (day + hour, 40),
            (day + 3 * hour, 1_000),
        ] {
            oracle
                .write_snapshot(ts, &[(pair, coen_iso(price), coen_iso(1))])
                .unwrap();
        }

        let snapshot = default_snapshot_at(day + 3 * hour + 1_800);
        assert_eq!(
            (snapshot.start(), snapshot.cutoff()),
            (day - 5 * hour, day + 3 * hour)
        );
        assert_eq!(
            oracle.finalized_window_vwap(pair, snapshot).unwrap(),
            Some(coen_iso(25))
        );
    });
}

#[test]
fn finalized_window_vwap_treats_a_zero_price_as_missing() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        oracle
            .write_snapshot(day + 5 * hour, &[(pair, U256::ZERO, coen_iso(1))])
            .unwrap();

        assert_eq!(
            oracle
                .finalized_window_vwap(pair, default_snapshot_at(day + 10 * hour))
                .unwrap(),
            None
        );
    });
}

#[test]
fn a_policy_change_leaves_an_old_snapshot_readable_and_unchanged() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour + 30 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        for (ts, price) in [(day + 3 * hour, 10), (day + 5 * hour, 40)] {
            oracle
                .write_snapshot(ts, &[(pair, coen_iso(price), coen_iso(1))])
                .unwrap();
        }
        let now = day + 10 * hour + 30 * 60;
        let before = default_snapshot_at(now);
        let price_before = oracle.finalized_window_vwap(pair, before).unwrap();

        let next_policy = crate::window::VwapPolicy {
            policy_version: 2,
            vwap_lookback_seconds: 21_600,
            ..crate::window::DEFAULT_VWAP_POLICY
        };
        let after = crate::window::get_vwap_snapshot_id(now, &next_policy).unwrap();

        assert_ne!(after, before);
        assert_eq!(after.start(), day + 4 * hour);
        assert_eq!(
            oracle.finalized_window_vwap(pair, after).unwrap(),
            Some(coen_iso(40))
        );
        assert_eq!(
            oracle.finalized_window_vwap(pair, before).unwrap(),
            price_before
        );
        assert_eq!(price_before, Some(coen_iso(25)));
    });
}

#[test]
fn get_policy_rate_reverts_for_an_unregistered_iso_code() {
    with_bare_oracle(|_storage, oracle| {
        crate::genesis::init_from_genesis(
            oracle,
            &crate::genesis::OracleGenesisConfig::default_config(),
        )
        .unwrap();
        let err = oracle.get_policy_rate(124).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("no policy rate for iso_code 124"),
            "unexpected error: {msg}"
        );
    });
}

/// The soft read that block hooks use when they walk the whole reference-currency
/// registry. The registry lists currencies independently of whether their COEN
/// pair is registered and priced. So "not priceable yet" must be reportable
/// without reverting and halting the block.
#[test]
fn coen_rate_for_opt_reports_unpriceable_currencies_instead_of_reverting() {
    with_bare_oracle(|storage, oracle| {
        // Never registered.
        assert_eq!(
            crate::api::coen_rate_for_opt(storage.clone(), 978).unwrap(),
            None
        );

        // Registered, but no rate published yet.
        oracle.register_pair(AddressPair::new_coen_to(978)).unwrap();
        assert_eq!(
            crate::api::coen_rate_for_opt(storage.clone(), 978).unwrap(),
            None
        );

        // Priced.
        crate::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            AddressPair::new_coen_to(978),
            crate::api::RateObservation {
                rate: U256::from(1234u64),
                block_number: 1,
                timestamp: 1,
            },
        )
        .unwrap();
        assert_eq!(
            crate::api::coen_rate_for_opt(storage, 978).unwrap(),
            Some(U256::from(1234u64))
        );
    });
}

#[test]
fn settlement_fx_rates_read_both_legs_from_the_one_required_snapshot() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour + 37 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        let eur = AddressPair::new_coen_to(978);
        oracle.register_pair(usd).unwrap();
        oracle.register_pair(eur).unwrap();
        for k in 2..10 {
            oracle
                .write_snapshot(
                    day + k * hour + 60,
                    &[
                        (usd, coen_iso(2), coen_iso(1)),
                        (eur, coen_iso(3), coen_iso(1)),
                    ],
                )
                .unwrap();
        }

        let fx = crate::api::settlement_fx_rates(storage.clone(), 978, 840)
            .unwrap()
            .expect("both legs are finalized for [02:00, 10:00)");
        let required = default_snapshot_at(day + 10 * hour + 37 * 60);
        assert_eq!(fx.snapshot, required);
        assert_eq!(fx.snapshot.start(), day + 2 * hour);
        assert_eq!(fx.snapshot.cutoff(), day + 10 * hour);
        assert_eq!(fx.issuance_currency_vwap_minor, coen_iso(3));
        assert_eq!(fx.reference_currency_vwap_minor, coen_iso(2));

        // A leg without a registered pair yields no rates at all, never a
        // one-sided or fallback quote.
        assert_eq!(
            crate::api::settlement_fx_rates(storage.clone(), 826, 840).unwrap(),
            None
        );
    });
}

#[test]
fn the_required_snapshot_identity_rolls_over_at_the_whole_hour_even_when_prices_are_equal() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    let seed = |storage: &outbe_primitives::storage::StorageHandle<'_>| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        let eur = AddressPair::new_coen_to(978);
        oracle.register_pair(usd).unwrap();
        oracle.register_pair(eur).unwrap();
        // Flat prices through the whole day: the numbers never move.
        for k in 0..12 {
            oracle
                .write_snapshot(
                    day + k * hour + 60,
                    &[
                        (usd, coen_iso(2), coen_iso(1)),
                        (eur, coen_iso(3), coen_iso(1)),
                    ],
                )
                .unwrap();
        }
    };

    let mut before = None;
    with_storage_at(day + 10 * hour + 59 * 60 + 59, |storage| {
        seed(&storage);
        let fx = crate::api::settlement_fx_rates(storage.clone(), 978, 840)
            .unwrap()
            .unwrap();
        assert_eq!(fx.snapshot.cutoff(), day + 10 * hour);
        before = Some(fx);
    });
    with_storage_at(day + 11 * hour, |storage| {
        seed(&storage);
        let fx = crate::api::settlement_fx_rates(storage.clone(), 978, 840)
            .unwrap()
            .unwrap();
        let before = before.unwrap();
        // Same numeric prices, different required context: an authorization
        // carrying the 10:00 id is stale at 11:00:00 exactly.
        assert_eq!(
            fx.issuance_currency_vwap_minor,
            before.issuance_currency_vwap_minor
        );
        assert_eq!(
            fx.reference_currency_vwap_minor,
            before.reference_currency_vwap_minor
        );
        assert_eq!(fx.snapshot.cutoff(), day + 11 * hour);
        assert_eq!(fx.snapshot.start(), day + 3 * hour);
        assert_ne!(fx.snapshot.to_u256(), before.snapshot.to_u256());
    });
}

#[test]
fn a_finalized_window_value_is_immutable_after_its_cutoff() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    let usd = AddressPair::new_coen_to(840);
    let snapshot = default_snapshot_at(day + 10 * hour + 5 * 60);

    let mut first = None;
    with_storage_at(day + 10 * hour + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(usd).unwrap();
        oracle
            .write_snapshot(day + 3 * hour, &[(usd, coen_iso(1), coen_iso(1))])
            .unwrap();
        oracle
            .write_snapshot(
                day + 9 * hour + 59 * 60 + 59,
                &[(usd, coen_iso(3), coen_iso(1))],
            )
            .unwrap();
        // An observation stamped exactly at the cutoff belongs to the next window.
        oracle
            .write_snapshot(day + 10 * hour, &[(usd, coen_iso(9), coen_iso(100))])
            .unwrap();
        first = oracle.finalized_window_vwap(usd, snapshot).unwrap();
        assert_eq!(first, Some(coen_iso(2)));
    });

    with_storage_at(day + 20 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(usd).unwrap();
        oracle
            .write_snapshot(day + 3 * hour, &[(usd, coen_iso(1), coen_iso(1))])
            .unwrap();
        oracle
            .write_snapshot(
                day + 9 * hour + 59 * 60 + 59,
                &[(usd, coen_iso(3), coen_iso(1))],
            )
            .unwrap();
        oracle
            .write_snapshot(day + 10 * hour, &[(usd, coen_iso(9), coen_iso(100))])
            .unwrap();
        // Ten more hours of very different prices after the cutoff.
        for k in 10..20 {
            oracle
                .write_snapshot(
                    day + k * hour + 30 * 60,
                    &[(usd, coen_iso(50), coen_iso(100))],
                )
                .unwrap();
        }
        assert_eq!(oracle.finalized_window_vwap(usd, snapshot).unwrap(), first);
    });
}

#[test]
fn an_authorization_naming_another_pricing_policy_never_matches_the_required_snapshot() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        oracle.register_pair(usd).unwrap();
        for k in 2..10 {
            oracle
                .write_snapshot(day + k * hour + 60, &[(usd, coen_iso(2), coen_iso(1))])
                .unwrap();
        }
        // The required snapshot is derived from block time and the active
        // policy only. This is the identity every direct settlement compares
        // its authorization against.
        let required = crate::api::current_vwap_snapshot(storage.clone()).unwrap();
        assert_eq!(required, default_snapshot_at(day + 10 * hour + 5 * 60));
        assert_eq!(required.policy(), crate::window::active_vwap_policy());

        // Same cutoff, well-formed ids, but they name a policy that is not the
        // chain's active one: a different version and a different lookback.
        // They can never equal the required identity, so the factories' snapshot
        // check rejects a direct settlement authorized under them.
        // The reader itself keeps pricing them: finalized history is readable
        // and unchanged across a policy change (see
        // a_policy_change_leaves_an_old_snapshot_readable_and_unchanged).
        let other_version = crate::window::VwapPolicy {
            policy_version: 2,
            ..crate::window::DEFAULT_VWAP_POLICY
        };
        let other_lookback = crate::window::VwapPolicy {
            vwap_lookback_seconds: 4 * hour,
            ..crate::window::DEFAULT_VWAP_POLICY
        };
        for foreign in [other_version, other_lookback] {
            let id =
                crate::window::get_vwap_snapshot_id(day + 10 * hour + 5 * 60, &foreign).unwrap();
            assert_eq!(id.cutoff(), required.cutoff());
            assert_ne!(id, required, "{foreign:?}");
            assert_ne!(id.to_u256(), required.to_u256(), "{foreign:?}");
            assert_eq!(
                crate::window::VwapSnapshotId::from_u256(id.to_u256()).unwrap(),
                id
            );
        }
    });
}
