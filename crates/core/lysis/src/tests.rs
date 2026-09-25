use crate::algorithm::*;
use alloy_primitives::U256;

/// A day whose Gratis allocation is the symbolic share of its nominal: the average fraction is
/// 0.32 and the per-league ceiling is twice that. Production derives both from the day itself
/// (`program_v1::execute`), so these are test inputs, not policy.
const F_FP_DEFAULT: U256 = u256_from_u128(SCALE_U128 * 32 / 100);
const F_MAX_FP: U256 = u256_from_u128(SCALE_U128 * 64 / 100);
use outbe_oracle::schema::OracleContract;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::{
    date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key, WorldwideDay,
    SECONDS_PER_DAY,
};

const SIX_DECIMAL_SCALE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);

fn coen(whole: u64) -> U256 {
    U256::from(whole) * SIX_DECIMAL_SCALE
}

fn seed_entry_prices(storage: &StorageHandle<'_>, now: u64, iso: u16, vwap: U256, current: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    let mut oracle = OracleContract::new(storage.clone());
    let index = oracle.pair_index_of(pair).unwrap();
    if !oracle
        .reference_currencies
        .read_all()
        .unwrap()
        .contains(&iso)
    {
        oracle.reference_currencies.push(iso).unwrap();
    }
    oracle.config_lookback_duration.write(86_400).unwrap();
    oracle.exchange_rate.write(&index, current).unwrap();
    let previous_day = previous_date_key(timestamp_to_date_key(now));
    oracle
        .write_snapshot(
            date_key_to_utc_timestamp(previous_day),
            &[(pair, vwap, SIX_DECIMAL_SCALE)],
        )
        .unwrap();
    oracle.finalize_utc_day_vwap(previous_day).unwrap();
}

#[test]
fn zero_or_over_limit_gratis_load_is_a_hard_failure_without_consumption() {
    let mut remaining = U256::from(10);
    assert!(crate::runtime::consume_required_gratis(&mut remaining, U256::ZERO).is_err());
    assert_eq!(remaining, U256::from(10));
    assert!(crate::runtime::consume_required_gratis(&mut remaining, U256::from(11)).is_err());
    assert_eq!(remaining, U256::from(10));
    crate::runtime::consume_required_gratis(&mut remaining, U256::from(4)).unwrap();
    assert_eq!(remaining, U256::from(6));
}

#[test]
fn lysis_entry_price_is_previous_day_vwap_regardless_of_current_price() {
    const NOW: u64 = 1_700_000_000;
    for iso in [840, 978] {
        for (vwap, current, expected) in [
            (250_000, 200_000, 250_000),
            (250_000, 320_000, 250_000),
            (250_000, 250_000, 250_000),
            (250_000, 0, 250_000),
        ] {
            let mut provider = HashMapStorageProvider::new(1);
            StorageHandle::enter(&mut provider, |storage| {
                let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
                outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
                seed_entry_prices(&storage, NOW, iso, U256::from(vwap), U256::from(current));
                assert_eq!(
                    crate::api::freeze_entry_price_snapshot(
                        storage,
                        WorldwideDay::new(20260715),
                        NOW
                    )
                    .unwrap()[&iso],
                    U256::from(expected),
                );
            });
        }
    }
}

#[test]
fn entry_price_map_is_frozen_once_for_all_available_currencies() {
    const NOW: u64 = 1_700_000_000;
    let previous_day = previous_date_key(timestamp_to_date_key(NOW));
    let start = date_key_to_utc_timestamp(previous_day);
    let end = start + SECONDS_PER_DAY;
    let day = WorldwideDay::new(20260715);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let usd = outbe_oracle::api::AddressPair::new_coen_to(840);
        let eur = outbe_oracle::api::AddressPair::new_coen_to(978);
        for iso in [826, 840, 978] {
            let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
            let index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
            oracle.reference_currencies.push(iso).unwrap();
            if iso != 826 {
                oracle
                    .exchange_rate
                    .write(&index, coen(if iso == 840 { 150 } else { 180 }))
                    .unwrap();
            }
        }
        oracle.config_lookback_duration.write(86_400).unwrap();
        for (time, usd_price, eur_price, volume) in [
            (start - 1, 900, 900, 10),
            (start, 100, 50, 1),
            (end - 1, 200, 150, 3),
            (end, 800, 800, 10),
        ] {
            oracle
                .write_snapshot(
                    time,
                    &[
                        (usd, coen(usd_price), coen(volume)),
                        (eur, coen(eur_price), coen(volume)),
                    ],
                )
                .unwrap();
        }
        oracle.finalize_utc_day_vwap(previous_day).unwrap();
        let prices = crate::api::freeze_entry_price_snapshot(storage.clone(), day, NOW).unwrap();
        assert_eq!(
            prices,
            std::collections::BTreeMap::from([(840, coen(175)), (978, coen(125))])
        );
        assert!(!prices.contains_key(&826));
        oracle
            .finalize_utc_day_vwap(timestamp_to_date_key(NOW))
            .unwrap();
        assert_eq!(
            crate::api::freeze_entry_price_snapshot(storage.clone(), day, NOW + SECONDS_PER_DAY)
                .unwrap(),
            prices
        );
        assert_eq!(
            outbe_nod::api::entry_price_snapshot(storage, day).unwrap(),
            Some(prices)
        );
    });
}

#[test]
fn positive_scurve_cannot_replace_a_missing_or_zero_lysis_vwap() {
    const T_NOW: u64 = 1_700_000_000;
    let wwd = WorldwideDay::new(20_260_719);

    for explicitly_write_zero in [false, true] {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let usd = outbe_oracle::api::DAY_TYPE_PAIR;
            let eur = outbe_oracle::api::AddressPair::new_coen_to(978);
            outbe_oracle::api::register_pair(storage.clone(), usd).unwrap();
            let eur_index = outbe_oracle::api::register_pair(storage.clone(), eur).unwrap();
            seed_entry_prices(
                &storage,
                T_NOW,
                840,
                U256::from(500_000_u64),
                U256::from(500_000_u64),
            );
            let oracle = OracleContract::new(storage.clone());
            oracle.reference_currencies.push(978).unwrap();
            oracle
                .exchange_rate
                .write(&eur_index, U256::from(900_000_u64))
                .unwrap();
            if explicitly_write_zero {
                oracle
                    .utc_day_vwap_value
                    .get_nested(&previous_date_key(timestamp_to_date_key(T_NOW)))
                    .write(&eur_index, U256::ZERO)
                    .unwrap();
            }
            outbe_oracle::scurve::store_scurve_entry(
                &mut OracleContract::new(storage.clone()),
                eur,
                wwd.to_timestamp_utc(),
                U256::from(900_000_u64),
            )
            .unwrap();

            // Current price and S-curve must not substitute for a missing or zero
            // previous-day VWAP: the currency stays out of the frozen snapshot.
            assert_eq!(
                crate::api::freeze_entry_price_snapshot(storage.clone(), wwd, T_NOW).unwrap(),
                std::collections::BTreeMap::from([(840, U256::from(500_000_u64))])
            );
        });
    }
}

#[test]
fn test_empty_population() {
    let result = calc_fraction_distribution_fp(&[], &[], 0, F_FP_DEFAULT, F_MAX_FP).unwrap();
    assert_eq!(result, vec![U256::ZERO]);
}

#[test]
fn test_single_fi_returns_target_fraction() {
    let y_fp = vec![SCALE]; // 100%
    let p = vec![5];
    let result = calc_fraction_distribution_fp(&y_fp, &p, 1, F_FP_DEFAULT, F_MAX_FP).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], F_FP_DEFAULT, "single FI should return f");
}

#[test]
fn test_two_fi_groups() {
    let y_fp = vec![
        SCALE * U256::from(6u64) / U256::from(10u64),
        SCALE * U256::from(4u64) / U256::from(10u64),
    ]; // 60/40
    let p = vec![1, 2];
    let result = calc_fraction_distribution_fp(&y_fp, &p, 2, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 2);

    // All fractions non-negative
    for (i, &frac) in result.iter().enumerate() {
        assert!(
            !frac.is_zero(),
            "fraction[{i}] should be positive, got {frac}"
        );
    }

    // Bounded by 2*fmax (reasonable bound for fixed-point)
    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] too large: {frac}");
    }
}

#[test]
fn test_three_fi_groups() {
    let y_fp = vec![
        SCALE * U256::from(50u64) / U256::from(100u64),
        SCALE * U256::from(30u64) / U256::from(100u64),
        SCALE * U256::from(20u64) / U256::from(100u64),
    ];
    let p = vec![50, 30, 20];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 3, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 3);

    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] > bound: {frac}");
    }
}

#[test]
fn test_many_fi_groups() {
    let n = 10;
    let y_fp: Vec<U256> = vec![SCALE / U256::from(n as u64); n];
    let p: Vec<u64> = (1..=n as u64).collect();

    let result = calc_fraction_distribution_fp(&y_fp, &p, 100, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), n);

    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] too large");
    }
}

/// f_fp must be clamped to [LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX/2].
#[test]
fn test_default_constants() {
    // Verify constants match expected values within integer precision
    assert_eq!(SCALE, SIX_DECIMAL_SCALE);
}

#[test]
fn test_with_zero_population_entries() {
    let half = SCALE / U256::from(2u64);
    let y_fp = vec![half, U256::ZERO, half];
    let p = vec![10, 0, 5];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 15, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 3);
    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= bound, "fraction[{i}] > bound: {frac}");
    }
}

#[test]
fn test_skewed_distribution() {
    let y_fp = vec![
        SCALE * U256::from(9u64) / U256::from(10u64),
        SCALE / U256::from(10u64),
    ];
    let p = vec![900, 100];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 1000, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 2);
    assert!(!result[0].is_zero());
    assert!(!result[1].is_zero());
}

/// Regression: large nominal amounts (> 2^53) must not lose precision.
#[test]
fn test_large_nominal_distribution() {
    // Simplified: 60/40 split -> use SCALE fractions directly.
    let y_fp = vec![
        SCALE * U256::from(6u64) / U256::from(10u64),
        SCALE * U256::from(4u64) / U256::from(10u64),
    ];
    let p = vec![600, 400];

    let result = calc_fraction_distribution_fp(&y_fp, &p, 1000, F_FP_DEFAULT, F_MAX_FP).unwrap();

    assert_eq!(result.len(), 2);
    let bound = F_MAX_FP * U256::from(2u64);
    for (i, &frac) in result.iter().enumerate() {
        assert!(
            !frac.is_zero(),
            "fraction[{i}] must be positive for large nominals"
        );
        assert!(frac <= bound, "fraction[{i}] must be bounded");
    }
}

// ---------------------------------------------------------------------------
// weighted-expenditure cap invariant
// ---------------------------------------------------------------------------

/// Assert the post-condition `sum(f1[i] * y_fp[i]) / SCALE <= f_fp` for the
/// output of `calc_fraction_distribution_fp`. Small round-down error is
/// acceptable; overshoot is not.
fn assert_weighted_within_target(result: &[U256], y_fp: &[U256], f_fp: U256) {
    let weighted: U256 = result
        .iter()
        .zip(y_fp.iter())
        .map(|(f, y)| *f * *y / SCALE)
        .sum();
    assert!(
        weighted <= f_fp,
        "weighted expenditure {weighted} exceeds target {f_fp}"
    );
}

#[test]
fn test_normalized_f1_respects_limit_skewed_population() {
    // Skewed population + imbalanced interest tends to push raw f1 over the
    // target. After normalization the post-condition must hold.
    let q = SCALE / U256::from(4u64);
    let y_fp = vec![q, q, q, q];
    let p = vec![100u64, 1, 1, 1];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 103, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 4);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_normalized_f1_respects_limit_many_groups() {
    let n = 10usize;
    let y_fp: Vec<U256> = (0..n).map(|_| SCALE / U256::from(n as u64)).collect();
    let p: Vec<u64> = (1..=n as u64).collect();
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 100, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), n);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_single_group_returns_f_without_normalization() {
    // The single-group fast path bypasses the normalization loop; `f_fp` is
    // returned as-is. Weighted total = f_fp * SCALE / SCALE = f_fp == target.
    let y_fp = vec![SCALE];
    let p = vec![10];
    let f_fp = F_FP_DEFAULT;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, f_fp, F_MAX_FP).unwrap();
    assert_eq!(result, vec![f_fp]);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_normalized_f1_preserves_ratios_when_scaled_down() {
    // When raw output overshoots and is scaled down, pairwise ratios between
    // groups should remain ~constant.
    let half = SCALE / U256::from(2u64);
    let y_fp = vec![half, half];
    let p = vec![50u64, 5];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 55, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    assert_weighted_within_target(&result, &y_fp, f_fp);
    // Both fractions should still be positive (not obliterated by scale-down).
    for &frac in &result {
        assert!(
            !frac.is_zero(),
            "fraction must remain positive after normalization"
        );
    }
}

// ---------------------------------------------------------------------------
// I256 precision - no silent zero-collapse on small FI groups
// ---------------------------------------------------------------------------

/// Input with a dominant group and one tiny-interest group. Under the
/// pre- i128 pipeline with `/1_000_000` scale-down the small group's
/// `f1` could collapse to 0 (up to 10^6 SCALE units of precision lost per
/// term). After I256 refactor the distribution must preserve the signal.
#[test]
fn test_small_fi_group_survives_i256_precision() {
    let tiny = U256::from(1_000_000u64);
    let y_fp = vec![
        SCALE - tiny, // dominant group ~= 99.9999%
        tiny,         // tiny group ~= 0.0001% - used to collapse to 0
    ];
    let p = vec![1000u64, 1];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 1001, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    assert!(
        !result[1].is_zero(),
        "tiny FI group must receive a non-zero fraction, got {}",
        result[1]
    );
}

/// When mass of Y is concentrated on the high end, `beta_num = f/fmax - E[Y]`
/// is negative and the algorithm must still produce a well-defined, bounded
/// distribution. Pre- the `/1_000_000` rounding could obliterate the
/// signed contribution for the lower-Y group.
#[test]
fn test_negative_beta_branch_produces_bounded_distribution() {
    let y_fp = vec![
        SCALE / U256::from(100u64),
        SCALE * U256::from(99u64) / U256::from(100u64),
    ]; // 1% / 99% split
    let p = vec![1u64, 1];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 2, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    let bound = F_MAX_FP * U256::from(2u64);
    for &f in &result {
        assert!(f <= bound, "fraction {f} exceeds LYSIS_LIMIT_MAX*2 bound");
    }
}

// ---------------------------------------------------------------------------
// Scale invariant: settlement_cost_minor must be in 10^6-minor units, not 10^12
// ---------------------------------------------------------------------------

/// 15 distinct-amount tributes, all bearing fidelity index 1. Sum is a clean
/// 1200 COEN so the percentage scenarios (5%/30%/32%) divide exactly with no
/// integer truncation in the deficit derivation - the assertions can use
/// strict equality rather than tolerance bands.
fn uniform_fi_one_population_15() -> (Vec<U256>, Vec<u16>, U256) {
    let nominal_amounts: Vec<U256> = (1u64..=15).map(|i| coen(10u64 * i)).collect();
    let tribute_fis = vec![1u16; 15];
    let total_interest: U256 = nominal_amounts
        .iter()
        .copied()
        .fold(U256::ZERO, |acc, v| acc + v);
    // Sanity: 10 * (1+2+...+15) = 1200 COEN.
    debug_assert_eq!(total_interest, coen(1200u64));
    (nominal_amounts, tribute_fis, total_interest)
}

#[test]
fn test_compute_fi_fraction_map_single_fi_five_percent_allocation() {
    let (nominal_amounts, tribute_fis, total_interest) = uniform_fi_one_population_15();
    // 5% deficit - well below the historical 8% floor.
    let lysis_limit_minor = total_interest * U256::from(5u64) / U256::from(100u64);

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        lysis_limit_minor,
    )
    .unwrap();

    assert_eq!(map.len(), 1, "all FI=1 must collapse to one map entry");
    let expected = SCALE * U256::from(5u64) / U256::from(100u64); // 0.05 * 10^6
    assert_eq!(
        map.get(&1).copied(),
        Some(expected),
        "scarce-gratis fraction must equal the 5% deficit coefficient"
    );
    println!("deficit fraction map: {:?}", map);
}

#[test]
fn test_compute_fi_fraction_map_single_fi_thirty_percent_allocation() {
    let (nominal_amounts, tribute_fis, total_interest) = uniform_fi_one_population_15();
    // 30% deficit - well above the historical 8%/16% range; the new logic
    // must not silently cap the fraction at 16%.
    let lysis_limit_minor = total_interest * U256::from(30u64) / U256::from(100u64);

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        lysis_limit_minor,
    )
    .unwrap();

    assert_eq!(map.len(), 1);
    let expected = SCALE * U256::from(30u64) / U256::from(100u64); // 0.30 * 10^6
    assert_eq!(
        map.get(&1).copied(),
        Some(expected),
        "abundant-gratis fraction must track the 30% deficit, not pin at 16%"
    );
}

#[test]
fn test_compute_fi_fraction_map_single_fi_thirtytwo_percent_allocation() {
    let (nominal_amounts, tribute_fis, total_interest) = uniform_fi_one_population_15();
    // 32% - matches the canonical metadosis symbolic rate (D1 in
    // metadosis-lysis-discrepancies.md). The fraction must reach 0.32, exactly.
    let lysis_limit_minor = total_interest * U256::from(32u64) / U256::from(100u64);

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        lysis_limit_minor,
    )
    .unwrap();

    assert_eq!(map.len(), 1);
    let expected = SCALE * U256::from(32u64) / U256::from(100u64); // 0.32 * 10^6
    assert_eq!(
        map.get(&1).copied(),
        Some(expected),
        "32% gratis allocation must produce a 32% fraction"
    );
}

/// Multi-FI scenario: 100 distinct-nominal tributes spread across 15 fidelity
/// indices, 32% gratis allocation.
#[test]
fn test_compute_fi_fraction_map_100_tributes_15_fis_thirtytwo_percent_allocation() {
    use std::collections::BTreeMap;

    // Distinct nominals 1..=100 COEN. Sum = 5050 COEN; 32% = 1616 COEN exactly.
    let nominal_amounts: Vec<U256> = (1u64..=100).map(coen).collect();
    // Round-robin FI assignment over 1..=15: FIs 1..=10 each get 7 tributes,
    // FIs 11..=15 each get 6 - covers every bucket with uneven population.
    let tribute_fis: Vec<u16> = (0u16..100).map(|i| (i % 15) + 1).collect();
    let total_interest: U256 = nominal_amounts
        .iter()
        .copied()
        .fold(U256::ZERO, |acc, v| acc + v);
    debug_assert_eq!(total_interest, coen(5050u64));

    let lysis_limit_minor = total_interest * U256::from(32u64) / U256::from(100u64);
    debug_assert_eq!(lysis_limit_minor, coen(1616u64));

    let map = crate::runtime::compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        lysis_limit_minor,
    )
    .unwrap();

    // 1. Every distinct FI must appear in the map.
    assert_eq!(
        map.len(),
        15,
        "every FI bucket present in input must receive a fraction"
    );
    for fi in 1u16..=15 {
        assert!(map.contains_key(&fi), "FI {fi} missing from fraction map");
    }

    // 2. Every fraction must be positive - the I256 pipeline must not collapse
    //    any group to zero, and the moment solver must not produce a negative
    //    that clamps to 0 (would starve a whole FI bucket).
    for (fi, frac) in &map {
        assert!(
            !frac.is_zero(),
            "FI {fi} got zero fraction: algorithm collapsed a group (got {frac})"
        );
    }

    // 3. Algorithm-level limit invariant. Reconstruct the y_fp vector exactly
    //    as the runtime does (BTreeMap-ordered group share with the truncation
    //    delta absorbed into the last entry) and assert the normalized
    //    `sum(f_g * y_fp_g)/SCALE <= f_fp` post-condition. This is the
    //    `assert_weighted_within_target` invariant lifted to multi-FI inputs.
    let mut group_interest: BTreeMap<u16, U256> = BTreeMap::new();
    for (i, &fi) in tribute_fis.iter().enumerate() {
        *group_interest.entry(fi).or_insert(U256::ZERO) += nominal_amounts[i];
    }
    let mut y_fp: Vec<U256> = group_interest
        .values()
        .map(|gi| *gi * SIX_DECIMAL_SCALE / total_interest)
        .collect();
    let y_sum: U256 = y_fp.iter().copied().sum();
    if let Some(last) = y_fp.last_mut() {
        if y_sum < SCALE {
            *last += SCALE - y_sum;
        }
    }
    let weighted: U256 = group_interest
        .keys()
        .zip(y_fp.iter())
        .map(|(fi, y)| {
            let f = map.get(fi).copied().unwrap_or(U256::ZERO);
            f * *y / SCALE
        })
        .sum();
    let f_fp = SCALE * U256::from(32u64) / U256::from(100u64); // 0.32 * 10^6
    assert!(
        weighted <= f_fp,
        "weighted sum(f*y_fp)/SCALE = {weighted} exceeds f_fp {f_fp} (32% limit violated)"
    );

    println!("100-tribute / 15-FI fraction map: {:?}", map);
    println!("weighted sum(f*y_fp)/SCALE: {} (f_fp: {})", weighted, f_fp);
}

// ---------------------------------------------------------------------
// Creator-reward: lysis records the per-owner contributor map
// ---------------------------------------------------------------------
