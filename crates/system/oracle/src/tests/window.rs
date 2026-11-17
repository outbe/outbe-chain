//! State-level tests: the 24-hour VWAP of a snapshot window and closed-day
//! history. Round coverage of the window lives in `coverage`.

use alloy_primitives::{Address, U256};

use crate::schema::OracleContract;

use super::common::*;

pub(super) fn default_snapshot_at(timestamp: u64) -> crate::window::VwapSnapshotId {
    crate::window::vwap_24h_snapshot_id(timestamp).unwrap()
}

#[test]
fn current_snapshot_dispatch_rejects_wide_timestamp_instead_of_panicking() {
    use alloy_sol_types::SolCall;
    use outbe_primitives::error::PrecompileError;

    with_storage_at(ATOMIC_DAY_START, |storage| {
        let call = crate::precompile::IOracle::vwap24hSnapshotIdCall {}.abi_encode();
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
                crate::api::current_vwap_24h_snapshot(storage.clone()),
                Err(PrecompileError::Revert(message)) if message == "timestamp exceeds u64"
            ));
        }
    });
}

#[test]
fn vwap_24h_weights_whole_window_volume() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 24 * hour + 37 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        note_hours(&mut oracle, day, day + 25 * hour);
        // Twelve hours at 1 x 1 and twelve at 3 x 9: (12 + 324) / 120 = 2.8,
        // not the unweighted 2.
        for k in 0..24 {
            let (price, volume) = if k < 12 { (1, 1) } else { (3, 9) };
            oracle
                .write_snapshot(
                    day + k * hour + 5,
                    &[(pair, coen_iso(price), coen_iso(volume))],
                )
                .unwrap();
        }

        let snapshot = default_snapshot_at(day + 24 * hour + 37 * 60);
        assert_eq!(
            oracle.vwap_24h(pair, snapshot).unwrap(),
            Some(U256::from(2_800_000u64))
        );
    });
}

#[test]
fn vwap_24h_is_half_open_and_counts_overlapping_hours_once() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 25 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        note_hours(&mut oracle, day, day + 26 * hour);
        // 44 exactly at 00:00, 20 in every hour 01..23, 68 exactly at 24:00.
        oracle
            .write_snapshot(day, &[(pair, coen_iso(44), coen_iso(1))])
            .unwrap();
        for k in 1..24 {
            oracle
                .write_snapshot(day + k * hour + 60, &[(pair, coen_iso(20), coen_iso(1))])
                .unwrap();
        }
        oracle
            .write_snapshot(day + 24 * hour, &[(pair, coen_iso(68), coen_iso(1))])
            .unwrap();

        // [00:00, 24:00): (44 + 23 * 20) / 24. [01:00, 25:00): (23 * 20 + 68) / 24.
        let at_24 = default_snapshot_at(day + 24 * hour);
        let at_25 = default_snapshot_at(day + 25 * hour);
        assert_eq!(oracle.vwap_24h(pair, at_24).unwrap(), Some(coen_iso(21)));
        assert_eq!(oracle.vwap_24h(pair, at_25).unwrap(), Some(coen_iso(22)));
    });
}

#[test]
fn vwap_24h_rejects_an_open_window_and_reports_an_empty_one() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 10 * hour + 59 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();

        let open = default_snapshot_at(day + 11 * hour);
        assert!(oracle.vwap_24h(pair, open).is_err());
        let closed = default_snapshot_at(day + 10 * hour);
        assert_eq!(oracle.vwap_24h(pair, closed).unwrap(), None);
        assert_eq!(
            crate::api::vwap_24h(storage.clone(), 978, closed).unwrap(),
            None
        );
    });
}

#[test]
fn vwap_24h_never_falls_back_to_an_earlier_window() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 25 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        note_hours(&mut oracle, day, day + 26 * hour);
        // Hours 00..15 cover [00:00, 24:00): sixteen of 24. Only fifteen of
        // them fall in [01:00, 25:00).
        for k in 0..16 {
            oracle
                .write_snapshot(day + k * hour + 1_800, &[(pair, coen_iso(7), coen_iso(1))])
                .unwrap();
        }

        assert_eq!(
            oracle
                .vwap_24h(pair, default_snapshot_at(day + 24 * hour))
                .unwrap(),
            Some(coen_iso(7))
        );
        assert_eq!(
            oracle
                .vwap_24h(pair, default_snapshot_at(day + 25 * hour))
                .unwrap(),
            None
        );
    });
}

#[test]
fn vwap_24h_spans_midnight() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    with_storage_at(day + 3 * hour + 1_800, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        note_hours(&mut oracle, day - 22 * hour, day + 4 * hour);
        // 16 in the first fifteen hours of the window, 40 in the last nine, and
        // 1,000 just outside the window on each side: (15 * 16 + 9 * 40) / 24.
        oracle
            .write_snapshot(day - 22 * hour, &[(pair, coen_iso(1_000), coen_iso(1))])
            .unwrap();
        for k in 0..24 {
            let price = if k < 15 { 16 } else { 40 };
            oracle
                .write_snapshot(
                    day - 21 * hour + k * hour,
                    &[(pair, coen_iso(price), coen_iso(1))],
                )
                .unwrap();
        }
        oracle
            .write_snapshot(day + 3 * hour, &[(pair, coen_iso(1_000), coen_iso(1))])
            .unwrap();

        let snapshot = default_snapshot_at(day + 3 * hour + 1_800);
        assert_eq!(
            (snapshot.start(), snapshot.cutoff()),
            (day - 21 * hour, day + 3 * hour)
        );
        assert_eq!(oracle.vwap_24h(pair, snapshot).unwrap(), Some(coen_iso(25)));
    });
}

#[test]
fn vwap_24h_treats_a_zero_price_as_missing() {
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
                .vwap_24h(pair, default_snapshot_at(day + 10 * hour))
                .unwrap(),
            None
        );
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
        assert_typed_revert(
            &err,
            crate::errors::OracleError::NoPolicyRate { iso_code: 124 },
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
    with_storage_at(day + 24 * hour + 37 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        let eur = AddressPair::new_coen_to(978);
        oracle.register_pair(usd).unwrap();
        oracle.register_pair(eur).unwrap();
        note_hours(&mut oracle, day, day + 25 * hour);
        for k in 0..24 {
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
            .expect("both legs are finalized for [00:00, 24:00)");
        let required = default_snapshot_at(day + 24 * hour + 37 * 60);
        assert_eq!(fx.snapshot, required);
        assert_eq!(fx.snapshot.start(), day);
        assert_eq!(fx.snapshot.cutoff(), day + 24 * hour);
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
        note_hours(&mut oracle, day, day + 26 * hour);
        // Flat prices through 25 hours: the numbers never move.
        for k in 0..25 {
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
    with_storage_at(day + 24 * hour + 59 * 60 + 59, |storage| {
        seed(&storage);
        let fx = crate::api::settlement_fx_rates(storage.clone(), 978, 840)
            .unwrap()
            .unwrap();
        assert_eq!(fx.snapshot.cutoff(), day + 24 * hour);
        before = Some(fx);
    });
    with_storage_at(day + 25 * hour, |storage| {
        seed(&storage);
        let fx = crate::api::settlement_fx_rates(storage.clone(), 978, 840)
            .unwrap()
            .unwrap();
        let before = before.unwrap();
        // Same numeric prices, different required context: an authorization
        // carrying the 24:00 id is stale at 25:00:00 exactly.
        assert_eq!(
            fx.issuance_currency_vwap_minor,
            before.issuance_currency_vwap_minor
        );
        assert_eq!(
            fx.reference_currency_vwap_minor,
            before.reference_currency_vwap_minor
        );
        assert_eq!(fx.snapshot.cutoff(), day + 25 * hour);
        assert_eq!(fx.snapshot.start(), day + hour);
        assert_ne!(fx.snapshot.to_u256(), before.snapshot.to_u256());
    });
}

#[test]
fn a_finalized_window_value_is_immutable_after_its_cutoff() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    let usd = AddressPair::new_coen_to(840);
    let snapshot = default_snapshot_at(day + 24 * hour + 5 * 60);

    let mut first = None;
    with_storage_at(day + 24 * hour + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(usd).unwrap();
        note_hours(&mut oracle, day, day + 25 * hour);
        // 1 in hours 00..11, 3 in hours 12..23, the last at 23:59:59.
        for k in 0..24 {
            let (price, offset) = if k < 12 { (1, 0) } else { (3, 59 * 60 + 59) };
            oracle
                .write_snapshot(
                    day + k * hour + offset,
                    &[(usd, coen_iso(price), coen_iso(1))],
                )
                .unwrap();
        }
        // An observation stamped exactly at the cutoff belongs to the next window.
        oracle
            .write_snapshot(day + 24 * hour, &[(usd, coen_iso(9), coen_iso(100))])
            .unwrap();
        first = oracle.vwap_24h(usd, snapshot).unwrap();
        assert_eq!(first, Some(coen_iso(2)));
    });

    with_storage_at(day + 34 * hour, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(usd).unwrap();
        note_hours(&mut oracle, day, day + 35 * hour);
        // 1 in hours 00..11, 3 in hours 12..23, the last at 23:59:59.
        for k in 0..24 {
            let (price, offset) = if k < 12 { (1, 0) } else { (3, 59 * 60 + 59) };
            oracle
                .write_snapshot(
                    day + k * hour + offset,
                    &[(usd, coen_iso(price), coen_iso(1))],
                )
                .unwrap();
        }
        oracle
            .write_snapshot(day + 24 * hour, &[(usd, coen_iso(9), coen_iso(100))])
            .unwrap();
        // Ten more hours of very different prices after the cutoff. They take
        // the cells of the first hours of the window, so the read uses the raw
        // snapshots of those hours.
        for k in 24..34 {
            oracle
                .write_snapshot(
                    day + k * hour + 30 * 60,
                    &[(usd, coen_iso(50), coen_iso(100))],
                )
                .unwrap();
        }
        assert_eq!(oracle.vwap_24h(usd, snapshot).unwrap(), first);
    });
}

#[test]
fn vwap_24h_set_tells_the_price_of_the_hour() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;

    use crate::precompile::IOracle;

    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    let usd = AddressPair::new_coen_to(840);
    let eur = AddressPair::new_coen_to(978);
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(1);
    provider.set_timestamp(U256::from(day + 24 * hour));
    StorageHandle::enter(&mut provider, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.register_pair(usd).unwrap();
        oracle.register_pair(eur).unwrap();
        note_hours(&mut oracle, day, day + 25 * hour);
        // USD has a price in each hour. EUR has no price, so it has no event.
        for k in 0..24 {
            oracle
                .write_snapshot(day + k * hour + 60, &[(usd, coen_iso(2), coen_iso(1))])
                .unwrap();
        }
        oracle.close_vwap_24h_hour(day + 24 * hour).unwrap();
    });

    let events: Vec<_> = provider
        .get_ordered_events()
        .iter()
        .filter_map(|log| IOracle::Vwap24hSet::decode_log(log).ok())
        .map(|event| event.data)
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].windowEnd, day + 24 * hour);
    assert_eq!(events[0].base, usd.address1());
    assert_eq!(events[0].quote, usd.address2());
    assert_eq!(events[0].vwap, coen_iso(2));
}
