//! State-level tests: finalized trailing-window VWAP, policy identity and
//! closed-day history. Round coverage of the window lives in `coverage`.

use alloy_primitives::{Address, U256};

use crate::schema::OracleContract;

use super::common::*;

pub(super) fn default_snapshot_at(timestamp: u64) -> crate::window::VwapSnapshotId {
    crate::window::get_vwap_snapshot_id(timestamp, &crate::window::DEFAULT_VWAP_POLICY).unwrap()
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
        let err = oracle.get_policy_rate(978).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("no policy rate for iso_code 978"),
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

fn seed_closed_days(oracle: &mut OracleContract, iso: u16, days: &[(u32, u64)], watermark: u32) {
    let index = oracle.register_pair(AddressPair::new_coen_to(iso)).unwrap();
    for &(day, vwap) in days {
        oracle
            .record_utc_day_vwap(day, index, U256::from(vwap))
            .unwrap();
    }
    oracle.utc_day_vwap_last_finalized.write(watermark).unwrap();
}

#[test]
fn closed_above_floor_needs_a_finalized_day_strictly_above_the_floor() {
    with_bare_oracle(|storage, oracle| {
        seed_closed_days(oracle, 840, &[(20260301, 100), (20260302, 150)], 20260302);
        oracle
            .record_utc_day_vwap(20260303u32, 1u32, U256::from(500u64))
            .unwrap();

        let crossed = |floor: u64| {
            crate::api::closed_above_floor(storage.clone(), 840, U256::from(floor), 20260301)
                .unwrap()
        };
        assert!(crossed(149));
        assert!(!crossed(150), "a day at the floor does not cross it");
        assert!(!crossed(200), "a day past the watermark is not read");
    });
}

#[test]
fn closed_above_floor_counts_only_days_from_the_first_full_one() {
    with_bare_oracle(|storage, oracle| {
        seed_closed_days(oracle, 840, &[(20260228, 500), (20260302, 100)], 20260302);

        let crossed = |from: u32| {
            crate::api::closed_above_floor(storage.clone(), 840, U256::from(200u64), from).unwrap()
        };
        assert!(
            crossed(20260228),
            "the walk steps over a missing day and a month boundary"
        );
        assert!(!crossed(20260301));
        assert!(!crossed(0));
    });
}

/// The month maximum follows its days through every write, including one that lowers the
/// day holding it.
#[test]
fn the_month_maximum_follows_its_days_through_every_write() {
    with_bare_oracle(|_storage, oracle| {
        seed_closed_days(oracle, 840, &[(20260303, 900), (20260317, 500)], 20260331);
        let month_max = |oracle: &OracleContract| {
            oracle
                .utc_month_vwap_max
                .get_nested(&202603u32)
                .read(&1u32)
                .unwrap()
        };
        assert_eq!(month_max(oracle), U256::from(900u64));
        oracle
            .record_utc_day_vwap(20260303, 1, U256::from(100u64))
            .unwrap();
        assert_eq!(
            month_max(oracle),
            U256::from(500u64),
            "lowering the maximum rereads the month"
        );
        oracle
            .record_utc_day_vwap(20260320, 1, U256::from(700u64))
            .unwrap();
        assert_eq!(month_max(oracle), U256::from(700u64));
        oracle
            .record_utc_day_vwap(20260317, 1, U256::from(10u64))
            .unwrap();
        assert_eq!(
            month_max(oracle),
            U256::from(700u64),
            "lowering another day keeps it"
        );
    });
}

/// Pseudo-random closed-day VWAPs for days 1, 9, 15, 28 and 31 of every month
/// of 2024 to 2026. Edge days outrank every other: a day past `watermark`
/// gets 99999 and 2025-06-01, a day before a start inside its month, gets
/// 88888.
fn pseudo_random_closed_days(watermark: u32) -> Vec<(u32, u64)> {
    let mut seed = 0x2545_f491_u64;
    let mut days = Vec::new();
    for year in 2024u32..=2026 {
        for month in 1u32..=12 {
            for dd in [1u32, 9, 15, 28, 31] {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                days.push((year * 10_000 + month * 100 + dd, 1 + (seed >> 33) % 10_000));
            }
        }
    }
    for (day, vwap) in days.iter_mut() {
        if *day > watermark {
            *vwap = 99_999;
        } else if *day == 20250601 {
            *vwap = 88_888;
        }
    }
    days
}

/// The largest VWAP among `days` from `from` up to `watermark`, or zero.
fn expected_max_since(days: &[(u32, u64)], from: u32, watermark: u32) -> U256 {
    days.iter()
        .filter(|(day, _)| *day >= from && *day <= watermark)
        .map(|(_, vwap)| U256::from(*vwap))
        .max()
        .unwrap_or(U256::ZERO)
}

/// Asserts that a closed day from `from` is above a floor just below
/// `expected`, and not above `expected` or a floor just above it.
fn assert_floor_checks_around(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    expected: U256,
    from: u32,
) {
    for (floor, above) in [
        (expected - U256::ONE, true),
        (expected, false),
        (expected + U256::ONE, false),
    ] {
        assert_eq!(
            crate::api::closed_above_floor(storage.clone(), 840, floor, from).unwrap(),
            above,
            "floor {floor} from {from}"
        );
    }
}

/// Month-bucketed reads return exactly what a walk over every day would, across months, years
/// and a day past the watermark.
#[test]
fn bounded_history_reads_match_a_walk_over_every_day() {
    with_bare_oracle(|storage, oracle| {
        let watermark = 20261215;
        let days = pseudo_random_closed_days(watermark);
        seed_closed_days(oracle, 840, &days, watermark);

        for from in [
            101, 20230101, 20240101, 20240131, 20240201, 20241231, 20250101, 20250615, 20251231,
            20261201, 20261215, 20261216, 20270101,
        ] {
            let expected = expected_max_since(&days, from, watermark);
            assert_eq!(
                crate::api::max_utc_day_vwap_since(storage.clone(), 840, from).unwrap(),
                expected,
                "max from {from}"
            );
            if expected.is_zero() {
                continue;
            }
            assert_floor_checks_around(&storage, expected, from);
        }
    });
}

#[test]
fn max_utc_day_vwap_since_reads_the_same_days_as_the_floor_check() {
    with_bare_oracle(|storage, oracle| {
        seed_closed_days(oracle, 840, &[(20260228, 500), (20260302, 150)], 20260302);
        oracle
            .record_utc_day_vwap(20260303u32, 1u32, U256::from(900u64))
            .unwrap();

        let max = |iso: u16, from: u32| {
            crate::api::max_utc_day_vwap_since(storage.clone(), iso, from).unwrap()
        };
        assert_eq!(max(840, 20260228), U256::from(500u64));
        assert_eq!(max(840, 20260301), U256::from(150u64));
        assert_eq!(
            max(840, 20260303),
            U256::ZERO,
            "a day past the watermark is not read"
        );
        assert_eq!(max(840, 0), U256::ZERO);
        assert_eq!(max(978, 20260228), U256::ZERO);
        for (from, floor) in [
            (20260228, 499u64),
            (20260228, 500),
            (20260301, 149),
            (20260301, 150),
        ] {
            assert_eq!(
                max(840, from) > U256::from(floor),
                crate::api::closed_above_floor(storage.clone(), 840, U256::from(floor), from)
                    .unwrap()
            );
        }
    });
}

#[test]
fn closed_above_floor_is_false_for_an_unregistered_currency() {
    with_bare_oracle(|storage, oracle| {
        seed_closed_days(oracle, 840, &[(20260301, 500)], 20260301);

        assert!(!crate::api::closed_above_floor(storage, 978, U256::from(1u64), 20260301).unwrap());
    });
}

// -- D02/D03 trailing-window contract: one snapshot for both legs, hourly
//    rollover of the authorization identity, immutability after cutoff, and
//    rejection of a snapshot id that names a different pricing policy. --------

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
