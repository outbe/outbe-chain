//! State-level tests: snapshot ring buffer, hourly cells and rolling VWAP.

use alloy_primitives::U256;

use crate::schema::{OracleContract, SCALE_1E18};

use super::common::*;

#[test]
fn write_snapshot_advances_the_ring_buffer_and_feeds_vwap() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        // Write 3 snapshots
        let entries = vec![(pair_key(COEN, USDT), fixed18(100), fixed18(1000))];
        oracle.write_snapshot(1000, &entries).unwrap();

        let entries2 = vec![(pair_key(COEN, USDT), fixed18(200), fixed18(2000))];
        oracle.write_snapshot(2000, &entries2).unwrap();

        let entries3 = vec![(pair_key(COEN, USDT), fixed18(300), fixed18(3000))];
        oracle.write_snapshot(3000, &entries3).unwrap();

        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 3);
        assert_eq!(oracle.snapshot_oldest_idx.read().unwrap(), 0);

        // Calculate VWAP over all snapshots
        // VWAP = (100*1000 + 200*2000 + 300*3000) / (1000 + 2000 + 3000)
        //      = (100000 + 400000 + 900000) / 6000
        //      = 1400000 / 6000
        //      = 233.333...
        let vwap = oracle
            .calculate_vwap(pair_key(COEN, USDT), 0, 5000)
            .unwrap();
        // Prices and volumes use the same 18-decimal scale; the quotient
        // retains the price scale after the weighted sum is divided by volume.
        let expected = fixed18(1_400_000) * SCALE_1E18 / fixed18(6_000);
        assert_eq!(vwap, expected);
    });
}

#[test]
fn calculate_vwap_includes_only_snapshots_inside_the_window() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        let entries1 = vec![(pair_key(COEN, USDT), fixed18(100), SCALE_1E18)];
        oracle.write_snapshot(1000, &entries1).unwrap();

        let entries2 = vec![(pair_key(COEN, USDT), fixed18(200), SCALE_1E18)];
        oracle.write_snapshot(2000, &entries2).unwrap();

        let entries3 = vec![(pair_key(COEN, USDT), fixed18(300), SCALE_1E18)];
        oracle.write_snapshot(3000, &entries3).unwrap();

        // VWAP from 1500..2500 should only include snapshot at 2000
        let vwap = oracle
            .calculate_vwap(pair_key(COEN, USDT), 1500, 2500)
            .unwrap();
        assert_eq!(vwap, fixed18(200));

        // VWAP from 2500..3500 should only include snapshot at 3000
        let vwap = oracle
            .calculate_vwap(pair_key(COEN, USDT), 2500, 3500)
            .unwrap();
        assert_eq!(vwap, fixed18(300));
    });
}

#[test]
fn calculate_vwap_excludes_the_half_open_end_boundary() {
    with_bare_coen840_oracle(|oracle, pair| {
        oracle
            .write_snapshot(1_000, &[(pair, coen_iso(10), coen_iso(1))])
            .unwrap();
        oracle
            .write_snapshot(2_000, &[(pair, coen_iso(900), coen_iso(1))])
            .unwrap();

        assert_eq!(
            oracle.calculate_vwap(pair, 1_000, 2_000).unwrap(),
            coen_iso(10)
        );
    });
}

#[test]
fn write_snapshot_updates_exact_prefix_and_suffix_aggregates() {
    with_bare_coen840_oracle(|oracle, pair| {
        let day = 1_780_012_800u64;

        for (offset, price, volume) in [
            (9 * 60 * 60 + 59 * 60, 1, 2),
            (10 * 60 * 60, 3, 4),
            (11 * 60 * 60 + 59 * 60, 5, 6),
            (12 * 60 * 60, 7, 8),
        ] {
            oracle
                .write_snapshot(day + offset, &[(pair, coen_iso(price), coen_iso(volume))])
                .unwrap();
        }

        let prefix_pv = oracle.wwd_prefix_pv_sum.get_nested(&pair);
        let prefix_vol = oracle.wwd_prefix_vol_sum.get_nested(&pair);
        let suffix_pv = oracle.wwd_suffix_pv_sum.get_nested(&pair);
        let suffix_vol = oracle.wwd_suffix_vol_sum.get_nested(&pair);
        assert_eq!(
            prefix_pv.read(&day).unwrap(),
            coen_iso(1) * coen_iso(2) + coen_iso(3) * coen_iso(4) + coen_iso(5) * coen_iso(6)
        );
        assert_eq!(prefix_vol.read(&day).unwrap(), coen_iso(12));
        assert_eq!(
            suffix_pv.read(&day).unwrap(),
            coen_iso(3) * coen_iso(4) + coen_iso(5) * coen_iso(6) + coen_iso(7) * coen_iso(8)
        );
        assert_eq!(suffix_vol.read(&day).unwrap(), coen_iso(18));
    });
}

#[test]
fn partial_aggregate_overflow_rolls_back_the_entire_snapshot() {
    with_bare_coen840_oracle(|oracle, pair| {
        let day = 1_780_012_800u64;
        oracle
            .wwd_prefix_pv_sum
            .get_nested(&pair)
            .write(&day, U256::MAX)
            .unwrap();

        let error = oracle
            .write_snapshot(day + 60 * 60, &[(pair, coen_iso(1), coen_iso(1))])
            .unwrap_err();
        assert!(error.to_string().contains("VWAP overflow"));
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 0);
        assert_eq!(
            oracle.daily_pv_sum.get_nested(&pair).read(&day).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            oracle
                .wwd_prefix_pv_sum
                .get_nested(&pair)
                .read(&day)
                .unwrap(),
            U256::MAX
        );
    });
}

#[test]
fn calculate_vwap_reverts_for_a_window_without_snapshots() {
    with_bare_oracle(|_storage, oracle| {
        // No snapshots at all
        assert!(oracle
            .calculate_vwap(pair_key(COEN, USDT), 0, 1000)
            .is_err());
    });
}

#[test]
fn calculate_vwap_treats_zero_volume_as_one_scaled_unit() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        // Zero-volume entries -> equal-weight averaging
        let entries1 = vec![(pair_key(COEN, USDT), fixed18(100), U256::ZERO)];
        oracle.write_snapshot(1000, &entries1).unwrap();

        let entries2 = vec![(pair_key(COEN, USDT), fixed18(200), U256::ZERO)];
        oracle.write_snapshot(2000, &entries2).unwrap();

        // Equal-weight: (100 + 200) / 2 = 150
        let vwap = oracle
            .calculate_vwap(pair_key(COEN, USDT), 0, 3000)
            .unwrap();
        // With zero volumes, each gets SCALE_1E18 weight:
        // sum(rate * 1e18) / sum(1e18) = (100*1e18 + 200*1e18) / (2*1e18) = 150
        let expected = (fixed18(100) * SCALE_1E18 + fixed18(200) * SCALE_1E18) / fixed18(2);
        assert_eq!(vwap, expected);
    });
}

#[test]
fn calculate_vwap_uses_the_six_decimal_sentinel_for_zero_volume_coen_iso() {
    with_bare_oracle(|_storage, oracle| {
        for (index, iso) in [840, 978].into_iter().enumerate() {
            let pair = AddressPair::new_coen_to(iso);
            oracle.register_pair(pair).unwrap();

            // This rate fits when multiplied by the COEN/ISO sentinel (1e6),
            // but overflows against the generic decimal18 sentinel.
            let rate = U256::MAX / COEN_ISO_SCALE;
            let timestamp = 1_000 + index as u64;
            oracle
                .write_snapshot(timestamp, &[(pair, rate, U256::ZERO)])
                .unwrap();

            assert_eq!(oracle.calculate_vwap(pair, timestamp, 2_000).unwrap(), rate);
        }
    });
}

#[test]
fn calculate_vwap_returns_a_six_decimal_coen_iso_price() {
    with_bare_oracle(|_storage, oracle| {
        oracle.register_pair(AddressPair::new_coen_to(840)).unwrap();
        oracle
            .write_snapshot(
                1_000,
                &[(pair_key(COEN, usd()), coen_iso(100), coen_iso(2))],
            )
            .unwrap();
        oracle
            .write_snapshot(
                2_000,
                &[(pair_key(COEN, usd()), coen_iso(200), coen_iso(1))],
            )
            .unwrap();

        assert_eq!(
            oracle
                .calculate_vwap(pair_key(COEN, usd()), 0, 3_000)
                .unwrap(),
            U256::from(133_333_333u64)
        );
    });
}

#[test]
fn calculate_vwap_isolates_each_pair_within_one_snapshot() {
    with_bare_coen_usdt_oracle(|_storage, oracle| {
        oracle
            .register_pair(AddressPair::from_addresses(ETH, USDT))
            .unwrap();

        let entries = vec![
            (pair_key(COEN, USDT), fixed18(1), fixed18(100)),
            (pair_key(ETH, USDT), fixed18(2000), fixed18(50)),
        ];
        oracle.write_snapshot(1000, &entries).unwrap();

        // VWAP for COEN should be 1
        let vwap_coen = oracle
            .calculate_vwap(pair_key(COEN, USDT), 0, 2000)
            .unwrap();
        assert_eq!(vwap_coen, SCALE_1E18);

        // VWAP for ETH should be 2000
        let vwap_eth = oracle.calculate_vwap(pair_key(ETH, USDT), 0, 2000).unwrap();
        assert_eq!(vwap_eth, fixed18(2000));
    });
}

#[test]
fn hourly_cells_reproduce_the_raw_snapshot_vwap() {
    with_bare_oracle(|_storage, oracle| {
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();

        let day = ATOMIC_DAY_START;
        let hour = 3_600;
        let gap = (day + 5 * hour)..(day + 8 * hour);
        let mut samples = Vec::new();
        let mut ts = day - 30 * hour + 17;
        let mut i = 0u64;
        while ts < day + 20 * hour {
            if !gap.contains(&ts) {
                let sample = (ts, coen_iso(100 + i % 17), coen_iso(1 + i % 5));
                oracle
                    .write_snapshot(ts, &[(pair, sample.1, sample.2)])
                    .unwrap();
                samples.push(sample);
            }
            ts += 1_337;
            i += 1;
        }

        for (start, end) in [
            (day + 10 * hour, day + 18 * hour),
            (day + 10 * hour + 1_020, day + 13 * hour + 2_460),
            (day + 3 * hour + 300, day + 3 * hour + 3_000),
            (day + 4 * hour, day + 9 * hour),
            (day - 26 * hour, day + 5 * hour + 1),
            (day - 30 * hour, day + 20 * hour),
            (day - 5 * hour, day + 3 * hour),
            (day - 5 * hour + 77, day + 3 * hour - 5),
            (day - 26 * hour, day - 2 * hour),
            (day - 30 * hour + 900, day - hour),
        ] {
            let (pv, volume) = samples
                .iter()
                .filter(|(ts, _, _)| (start..end).contains(ts))
                .fold((U256::ZERO, U256::ZERO), |(pv, v), (_, price, vol)| {
                    (pv + price * vol, v + vol)
                });
            assert_eq!(
                oracle.calculate_vwap(pair, start, end).unwrap(),
                pv / volume,
                "window [{start}, {end})"
            );
        }
    });
}

/// One raw snapshot sample: pair, timestamp, rate and volume.
type Sample = (AddressPair, u64, U256, U256);

/// Writes 60 snapshots 1111 s apart from `ATOMIC_DAY_START`. Each holds a
/// `coen` entry (zero volume on every fourth), and every hour except each
/// third one also holds an `eth` entry. Returns every written entry.
fn write_sample_snapshots(
    oracle: &mut OracleContract<'_>,
    coen: AddressPair,
    eth: AddressPair,
) -> Vec<Sample> {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    let mut samples: Vec<Sample> = Vec::new();
    for i in 0..60u64 {
        let ts = day + i * 1_111;
        let coen_volume = if i % 4 == 0 {
            U256::ZERO
        } else {
            coen_iso(i % 7)
        };
        let mut entries = vec![(coen, coen_iso(100 + i % 9), coen_volume)];
        if (ts - day) / hour % 3 != 1 {
            entries.push((eth, fixed18(2_000 + i), fixed18(i % 3)));
        }
        oracle.write_snapshot(ts, &entries).unwrap();
        samples.extend(entries.into_iter().map(|(p, r, v)| (p, ts, r, v)));
    }
    samples
}

/// The volume-weighted average rate of `pair` over the `samples` in
/// `[start, end)`, with zero volume weighted as the Oracle weights it. `None`
/// without weight.
fn reference_vwap(samples: &[Sample], pair: AddressPair, start: u64, end: u64) -> Option<U256> {
    let (pv, volume) = samples
        .iter()
        .filter(|(p, ts, _, _)| *p == pair && (start..end).contains(ts))
        .fold(
            (U256::ZERO, U256::ZERO),
            |(pv, total), (_, _, rate, vol)| {
                let weight = if vol.is_zero() {
                    crate::constants::zero_volume_weight(pair)
                } else {
                    *vol
                };
                (pv + rate * weight, total + weight)
            },
        );
    (!volume.is_zero()).then(|| pv / volume)
}

#[test]
fn hourly_cells_reproduce_raw_vwap_for_several_pairs_and_zero_volume() {
    with_bare_oracle(|_storage, oracle| {
        let coen = AddressPair::new_coen_to(840);
        let eth = AddressPair::from_addresses(ETH, USDT);
        oracle.register_pair(coen).unwrap();
        oracle.register_pair(eth).unwrap();

        let day = ATOMIC_DAY_START;
        let hour = 3_600;
        let samples = write_sample_snapshots(oracle, coen, eth);

        for pair in [coen, eth] {
            for (start, end) in [
                (day + hour, day + 15 * hour),
                (day + 1_234, day + 17 * hour + 99),
                (day + 4 * hour, day + 5 * hour),
            ] {
                let expected = reference_vwap(&samples, pair, start, end);
                assert_eq!(
                    oracle.try_calculate_vwap(pair, start, end).unwrap(),
                    expected,
                    "{pair:?} [{start}, {end})"
                );
            }
        }
    });
}

#[test]
fn a_snapshot_cannot_precede_the_previous_one() {
    with_bare_coen840_oracle(|oracle, pair| {
        let ts = ATOMIC_DAY_START + 5 * 3_600;
        let entry = [(pair, coen_iso(10), coen_iso(1))];
        oracle.write_snapshot(ts, &entry).unwrap();
        oracle.write_snapshot(ts, &entry).unwrap();

        let err = oracle.write_snapshot(ts - 1, &entry).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::OracleError::SnapshotOutOfOrder
            )
            .to_string()
        );
        assert_eq!(oracle.snapshot_write_idx.read().unwrap(), 2);
    });
}

#[test]
fn an_hourly_cell_serves_a_whole_hour_whose_raw_snapshots_were_evicted() {
    with_bare_coen840_oracle(|oracle, pair| {
        let start = ATOMIC_DAY_START + 11 * 3_600;
        oracle
            .write_snapshot(start, &[(pair, coen_iso(10), coen_iso(1))])
            .unwrap();
        oracle
            .write_snapshot(start + 60, &[(pair, coen_iso(20), coen_iso(1))])
            .unwrap();
        oracle.snapshot_oldest_idx.write(1).unwrap();

        assert_eq!(
            oracle.calculate_vwap(pair, start, start + 3_600).unwrap(),
            coen_iso(15)
        );
    });
}

/// The bulk calculators skip pairs that hold no samples. But a rejected argument
/// is not "no data". It has to reach the caller, and the empty-result path must
/// not absorb it.
#[test]
fn bulk_calculators_propagate_argument_errors_instead_of_reporting_no_data() {
    with_coen_usdt_oracle(|_storage, oracle| {
        oracle
            .write_snapshot(1000, &[(pair_key(COEN, USDT), fixed18(1), fixed18(100))])
            .unwrap();

        let err = oracle.calculate_twaps(2000, 0).unwrap_err();
        assert!(
            err.to_string().contains("lookback_seconds"),
            "zero lookback must surface as an argument error, got {err:?}"
        );

        let err = oracle.calculate_vwaps(2000, 2000).unwrap_err();
        assert!(
            err.to_string().contains("start_time must be less than"),
            "empty range must surface as an argument error, got {err:?}"
        );

        // A well-formed window with no samples still reports the no-data revert.
        let err = oracle.calculate_vwaps(5000, 6000).unwrap_err();
        assert!(
            err.to_string().contains("no VWAP data"),
            "empty window must stay a no-data revert, got {err:?}"
        );
    });
}
