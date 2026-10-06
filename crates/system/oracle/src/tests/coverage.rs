//! Finalized-window coverage (E01). An hour counts only when it has two thirds
//! of the tally rounds that its blocks allowed. The hours that count must hold
//! two thirds of the rounds of the whole window.

use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;

use crate::schema::OracleContract;

use super::common::*;
use super::window::default_snapshot_at;

/// Fills `[start, start + 8h)` with `count` evenly spaced snapshots for `pair`
/// and records 1,800 blocks per hour. With `vote_period = 8` the window allows
/// exactly 1,800 rounds. `sparse_pair` joins every 18th snapshot.
fn fill_window(
    oracle: &mut OracleContract<'_>,
    pair: AddressPair,
    start: u64,
    count: u64,
    sparse_pair: Option<AddressPair>,
) {
    let hour = 3_600;
    oracle.config_vote_period.write(8).unwrap();
    for k in 0..=8 {
        oracle
            .record_hour_block(start + k * hour, 1_000 + k * 1_800)
            .unwrap();
    }
    for i in 0..count {
        let timestamp = start + i * (8 * hour) / count;
        let mut entries = vec![(pair, coen_iso(2), coen_iso(1))];
        if let Some(other) = sparse_pair.filter(|_| i % 18 == 0) {
            entries.push((other, coen_iso(5), coen_iso(1)));
        }
        oracle.write_snapshot(timestamp, &entries).unwrap();
    }
}

#[test]
fn finalized_window_vwap_requires_two_thirds_round_coverage() {
    let day = ATOMIC_DAY_START;
    let hour = 3_600;
    let start = day + 2 * hour;
    let cutoff = start + 8 * hour;

    // 1,300 of 1,800 possible rounds (72%) is enough. A pair present in only
    // 73 of them is not enough. The judgement is per pair.
    with_storage_at(cutoff + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        let eur = AddressPair::new_coen_to(978);
        oracle.register_pair(usd).unwrap();
        oracle.register_pair(eur).unwrap();
        fill_window(&mut oracle, usd, start, 1_300, Some(eur));
        let snapshot = default_snapshot_at(cutoff + 5 * 60);
        assert_eq!(
            oracle.finalized_window_vwap(usd, snapshot).unwrap(),
            Some(coen_iso(2))
        );
        assert_eq!(oracle.finalized_window_vwap(eur, snapshot).unwrap(), None);
        // The raw rolling read still sees the sparse pair. The gate applies only
        // to the finalized window.
        assert_eq!(
            oracle.calculate_vwap(eur, start, cutoff).unwrap(),
            coen_iso(5)
        );
    });

    // 1,100 of 1,800 (61%) is not enough even though the price is positive.
    with_storage_at(cutoff + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        oracle.register_pair(usd).unwrap();
        fill_window(&mut oracle, usd, start, 1_100, None);
        let snapshot = default_snapshot_at(cutoff + 5 * 60);
        assert_eq!(oracle.finalized_window_vwap(usd, snapshot).unwrap(), None);
        assert_eq!(
            oracle.calculate_vwap(usd, start, cutoff).unwrap(),
            coen_iso(2)
        );
    });

    // Exactly two thirds passes. One round fewer fails.
    with_storage_at(cutoff + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        oracle.register_pair(usd).unwrap();
        fill_window(&mut oracle, usd, start, 1_200, None);
        let snapshot = default_snapshot_at(cutoff + 5 * 60);
        assert!(oracle
            .finalized_window_vwap(usd, snapshot)
            .unwrap()
            .is_some());
    });
    with_storage_at(cutoff + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = AddressPair::new_coen_to(840);
        oracle.register_pair(usd).unwrap();
        fill_window(&mut oracle, usd, start, 1_199, None);
        let snapshot = default_snapshot_at(cutoff + 5 * 60);
        assert_eq!(oracle.finalized_window_vwap(usd, snapshot).unwrap(), None);
    });
}

#[test]
fn hour_first_block_is_recorded_once_per_hour() {
    let day = ATOMIC_DAY_START;
    with_storage_at(day, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        oracle.record_hour_block(day + 10, 100).unwrap();
        oracle.record_hour_block(day + 3_599, 900).unwrap();
        oracle.record_hour_block(day + 3_600, 901).unwrap();
        assert_eq!(oracle.hour_first_block.read(&day).unwrap(), 100);
        assert_eq!(oracle.hour_first_block.read(&(day + 3_600)).unwrap(), 901);
    });
}

/// Records 1,800 blocks per hour (225 rounds at `vote_period = 8`) and writes
/// `per_hour[i]` evenly spaced snapshots at `prices[i]` into hour `i` of the
/// eight-hour window that starts at `start`.
fn fill_hours(
    oracle: &mut OracleContract<'_>,
    pair: AddressPair,
    start: u64,
    per_hour: [u64; 8],
    prices: [u64; 8],
) {
    let hour = 3_600;
    oracle.config_vote_period.write(8).unwrap();
    for k in 0..=8 {
        oracle
            .record_hour_block(start + k * hour, 1_000 + k * 1_800)
            .unwrap();
    }
    for (index, (&count, &price)) in per_hour.iter().zip(&prices).enumerate() {
        for i in 0..count {
            let timestamp = start + index as u64 * hour + i * hour / count;
            oracle
                .write_snapshot(timestamp, &[(pair, coen_iso(price), coen_iso(1))])
                .unwrap();
        }
    }
}

fn window_vwap_for(per_hour: [u64; 8], prices: [u64; 8]) -> Option<U256> {
    let start = ATOMIC_DAY_START + 2 * 3_600;
    let cutoff = start + 8 * 3_600;
    let mut result = None;
    with_storage_at(cutoff + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        fill_hours(&mut oracle, pair, start, per_hour, prices);
        result = oracle
            .finalized_window_vwap(pair, default_snapshot_at(cutoff + 5 * 60))
            .unwrap();
    });
    result
}

#[test]
fn finalized_window_vwap_leaves_out_hours_below_two_thirds() {
    // Hour 3 has 149 of 225 rounds, one short of two thirds, at a wild price.
    // The VWAP leaves it out entirely. The seven full hours carry the window
    // (1,575 of 1,800), and the price of hour 3 never reaches the VWAP.
    assert_eq!(
        window_vwap_for(
            [225, 225, 225, 149, 225, 225, 225, 225],
            [2, 2, 2, 90, 2, 2, 2, 2]
        ),
        Some(coen_iso(2))
    );
    // With 150 rounds the same hour counts and pulls the average.
    let pulled = window_vwap_for(
        [225, 225, 225, 150, 225, 225, 225, 225],
        [2, 2, 2, 90, 2, 2, 2, 2],
    )
    .unwrap();
    assert!(pulled > coen_iso(2));

    // A lone snapshot in an otherwise empty hour is the M-13 case: ignored.
    assert_eq!(
        window_vwap_for(
            [225, 225, 225, 1, 225, 225, 225, 225],
            [2, 2, 2, 90, 2, 2, 2, 2]
        ),
        Some(coen_iso(2))
    );
}

#[test]
fn finalized_window_vwap_needs_two_thirds_of_the_window_from_counting_hours() {
    let twos = [2; 8];
    // Six full hours and two empty ones: 1,350 of 1,800.
    assert_eq!(
        window_vwap_for([225, 225, 0, 225, 225, 0, 225, 225], twos),
        Some(coen_iso(2))
    );
    // Five full hours: 1,125 of 1,800.
    assert_eq!(
        window_vwap_for([225, 225, 0, 225, 0, 0, 225, 225], twos),
        None
    );
    // Every one of six hours passes on its own (160 of 225), yet together they
    // hold 960 of 1,800: each hour is fine, the window is not.
    assert_eq!(
        window_vwap_for([160, 160, 0, 160, 160, 0, 160, 160], twos),
        None
    );
    // Seven hours at 172 (1,204) clear the window. At 171 (1,197) they do not.
    assert_eq!(
        window_vwap_for([172, 172, 172, 0, 172, 172, 172, 172], twos),
        Some(coen_iso(2))
    );
    assert_eq!(
        window_vwap_for([171, 171, 171, 0, 171, 171, 171, 171], twos),
        None
    );
    // Hours that fail individually do not rescue the total: 8 x 149 = 1,192
    // snapshots exist, but none of them counts.
    assert_eq!(window_vwap_for([149; 8], twos), None);
}

#[test]
fn finalized_window_vwap_skips_hours_without_blocks() {
    // The chain produced no block in hours 2 and 3: those hours allow no round,
    // so the window's span shrinks to six hours and full coverage of them passes.
    let start = ATOMIC_DAY_START + 2 * 3_600;
    let cutoff = start + 8 * 3_600;
    with_storage_at(cutoff + 5 * 60, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair = AddressPair::new_coen_to(840);
        oracle.register_pair(pair).unwrap();
        oracle.config_vote_period.write(8).unwrap();
        let mut block = 1_000;
        for k in 0..=8u64 {
            if k == 2 || k == 3 {
                continue;
            }
            oracle.record_hour_block(start + k * 3_600, block).unwrap();
            if k < 8 {
                for i in 0..225 {
                    oracle
                        .write_snapshot(
                            start + k * 3_600 + i * 16,
                            &[(pair, coen_iso(3), coen_iso(1))],
                        )
                        .unwrap();
                }
            }
            block += 1_800;
        }
        assert_eq!(
            oracle
                .finalized_window_vwap(pair, default_snapshot_at(cutoff + 5 * 60))
                .unwrap(),
            Some(coen_iso(3))
        );
    });
}
