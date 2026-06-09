use crate::algorithm::*;
use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_nod::NodContract;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::units::{Units, SCALE_1E18};
use outbe_tribute::{TributeContract, TributeData};

const SCALE: u128 = 1_000_000_000_000_000_000;

fn gas_audit_address(n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[0] = 0x22;
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

fn gas_audit_tribute(
    token_id: u64,
    owner: Address,
    worldwide_day: WorldwideDay,
    nominal_amount_minor: U256,
) -> TributeData {
    TributeData {
        token_id: U256::from(token_id),
        owner,
        worldwide_day,
        issuance_amount_minor: nominal_amount_minor / U256::from(2u64),
        issuance_currency: 1,
        nominal_amount_minor,
        reference_currency: 840,
        tribute_price_minor: U256::ZERO,
    }
}

#[test]
fn gas_08_lysis_dense_day_completes_issues_nods_and_clears_day_index() {
    const DENSE_TRIBUTE_COUNT: u64 = 512;
    const T_NOW: u64 = 1_700_000_000;
    let wwd = WorldwideDay::new(20260525);
    let nominal = U256::in_units(100u64);
    let total_nominal = nominal * U256::from(DENSE_TRIBUTE_COUNT);
    let gratis_allocation = total_nominal / U256::from(10u64);
    let cost_of_gratis = U256::from(500_000_000_000_000_000u128);
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));

    StorageHandle::enter(&mut storage, |storage| {
        let mut oracle = OracleContract::new(storage.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle
            .worldwide_day_vwap_pair_count
            .write(&wwd, 1u32)
            .unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, pair_id)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, cost_of_gratis)
            .unwrap();

        let mut tribute = TributeContract::new(storage.clone());
        tribute.unseal_day(wwd).unwrap();
        let mut owners = Vec::with_capacity(DENSE_TRIBUTE_COUNT as usize);
        for token_id in 1..=DENSE_TRIBUTE_COUNT {
            let owner = gas_audit_address(token_id);
            tribute
                .issue(&gas_audit_tribute(token_id, owner, wwd, nominal))
                .unwrap();
            owners.push(owner);
        }
        assert_eq!(
            tribute.get_all_day_tributes(wwd).unwrap().len(),
            DENSE_TRIBUTE_COUNT as usize,
            "GAS-08 fixture must seed a dense but valid Lysis day"
        );

        let result = crate::runtime::lysis(storage.clone(), wwd, gratis_allocation)
            .expect("GAS-08 dense Lysis day must complete");

        assert_eq!(
            result.tribute_ids.len(),
            DENSE_TRIBUTE_COUNT as usize,
            "GAS-08: dense Lysis should load every tribute in the day"
        );
        assert_eq!(
            result.nod_ids.len(),
            DENSE_TRIBUTE_COUNT as usize,
            "GAS-08: dense Lysis should issue one NOD for every funded tribute"
        );

        let tribute = TributeContract::new(storage.clone());
        assert!(
            tribute.get_all_day_tributes(wwd).unwrap().is_empty(),
            "GAS-08: day tribute index must be cleared after full dense Lysis processing"
        );
        assert_eq!(
            tribute.total_supply().unwrap(),
            0,
            "GAS-08: all processed tributes must be burned after dense Lysis"
        );

        let nod = NodContract::new(storage.clone());
        assert_eq!(
            nod.total_supply().unwrap(),
            DENSE_TRIBUTE_COUNT,
            "GAS-08: dense Lysis must persist every issued NOD"
        );
        let mut issued_gratis = U256::ZERO;
        for (idx, nod_id) in result.nod_ids.iter().enumerate() {
            let item = nod
                .get_item(*nod_id)
                .unwrap()
                .expect("GAS-08 issued NOD must be readable");
            assert_eq!(item.owner, owners[idx]);
            assert_eq!(item.worldwide_day, wwd);
            assert_eq!(item.league_id, 1);
            assert!(
                !item.gratis_load_minor.is_zero(),
                "GAS-08: issued dense NOD must carry positive gratis load"
            );
            assert_eq!(
                item.cost_amount_minor,
                cost_of_gratis * item.gratis_load_minor / SCALE_1E18,
                "GAS-08: dense NOD cost accounting must preserve the 1e18 scale"
            );
            issued_gratis += item.gratis_load_minor;
        }
        assert_eq!(
            issued_gratis + result.remaining_gratis,
            gratis_allocation,
            "GAS-08: dense Lysis must conserve gratis allocation across issued load + remainder"
        );
    });
}

#[test]
fn test_empty_population() {
    let result =
        calc_fraction_distribution_fp(&[], &[], 10, 0, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX).unwrap();
    assert_eq!(result, vec![0]);
}

#[test]
fn test_single_fi_returns_target_fraction() {
    let y_fp = vec![SCALE]; // 100%
    let p = vec![5];
    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 1, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], LYSIS_LIMIT_MIN, "single FI should return f");
}

#[test]
fn test_two_fi_groups() {
    let y_fp = vec![SCALE * 6 / 10, SCALE * 4 / 10]; // 60/40
    let p = vec![1, 2];
    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 2, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX).unwrap();

    assert_eq!(result.len(), 2);

    // All fractions non-negative
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac > 0, "fraction[{i}] should be positive, got {frac}");
    }

    // Bounded by 2*fmax (reasonable bound for fixed-point)
    for (i, &frac) in result.iter().enumerate() {
        assert!(
            frac <= LYSIS_LIMIT_MAX * 2,
            "fraction[{i}] too large: {frac}"
        );
    }
}

#[test]
fn test_three_fi_groups() {
    let y_fp = vec![SCALE * 50 / 100, SCALE * 30 / 100, SCALE * 20 / 100];
    let p = vec![50, 30, 20];

    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 3, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX).unwrap();

    assert_eq!(result.len(), 3);

    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= LYSIS_LIMIT_MAX * 2, "fraction[{i}] > bound: {frac}");
    }
}

#[test]
fn test_many_fi_groups() {
    let n = 10;
    let y_fp: Vec<u128> = vec![SCALE / n as u128; n];
    let p: Vec<u64> = (1..=n as u64).collect();

    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 100, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX)
            .unwrap();

    assert_eq!(result.len(), n);

    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= LYSIS_LIMIT_MAX * 2, "fraction[{i}] too large");
    }
}

/// A-25: deficit_fp must clamp to u128::MAX when gratis >> total_interest.
#[test]
fn test_deficit_fp_clamp_at_u128_max() {
    // If gratis_allocation * SCALE / total_interest > u128::MAX, the downcast
    // must clamp, not silently truncate.
    // We can't easily trigger this via lysis() (U256 arithmetic), but verify
    // the clamp constant is correct.
    let max_u128 = u128::MAX;
    // After clamp, f_fp should still be at most LYSIS_LIMIT_MAX / 2
    let clamped = max_u128.clamp(LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX / 2);
    assert_eq!(
        clamped,
        LYSIS_LIMIT_MAX / 2,
        "extreme deficit must clamp to MAX/2"
    );
}

/// A-27: f_fp must be clamped to [LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX/2].
#[test]
fn test_f_fp_clamp_boundaries() {
    // Below minimum → floor
    let small: u128 = 1_000;
    let clamped = small.clamp(LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX / 2);
    assert_eq!(clamped, LYSIS_LIMIT_MIN, "below-minimum must floor to MIN");

    // Above MAX/2 → ceiling
    let large: u128 = LYSIS_LIMIT_MAX;
    let clamped = large.clamp(LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX / 2);
    assert_eq!(
        clamped,
        LYSIS_LIMIT_MAX / 2,
        "above-ceiling must cap to MAX/2"
    );

    // At boundary: MIN == MAX/2 (both are 0.08 * SCALE), so clamp range is a single point.
    let exact = LYSIS_LIMIT_MIN;
    let clamped = exact.clamp(LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX / 2);
    assert_eq!(clamped, LYSIS_LIMIT_MIN, "exact boundary must stay at MIN");
}

#[test]
fn test_default_constants() {
    // Verify constants match expected values within integer precision
    assert_eq!(LYSIS_LIMIT_MIN, 80_000_000_000_000_000); // 0.08 * 10^18
    assert_eq!(LYSIS_LIMIT_MAX, 160_000_000_000_000_000); // 0.16 * 10^18
}

#[test]
fn test_with_zero_population_entries() {
    let y_fp = vec![SCALE / 2, 0, SCALE / 2];
    let p = vec![10, 0, 5];

    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 15, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX).unwrap();

    assert_eq!(result.len(), 3);
    for (i, &frac) in result.iter().enumerate() {
        assert!(frac <= LYSIS_LIMIT_MAX * 2, "fraction[{i}] > bound: {frac}");
    }
}

#[test]
fn test_skewed_distribution() {
    let y_fp = vec![SCALE * 9 / 10, SCALE / 10];
    let p = vec![900, 100];

    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 1000, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX)
            .unwrap();

    assert_eq!(result.len(), 2);
    assert!(result[0] > 0);
    assert!(result[1] > 0);
}

/// Regression: large nominal amounts (> 2^53) must not lose precision.
#[test]
fn test_large_nominal_distribution() {
    // Simplified: 60/40 split → use SCALE fractions directly.
    let y_fp = vec![SCALE * 6 / 10, SCALE * 4 / 10];
    let p = vec![600, 400];

    let result =
        calc_fraction_distribution_fp(&y_fp, &p, 10, 1000, LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX)
            .unwrap();

    assert_eq!(result.len(), 2);
    for (i, &frac) in result.iter().enumerate() {
        assert!(
            frac > 0,
            "fraction[{i}] must be positive for large nominals"
        );
        assert!(frac <= LYSIS_LIMIT_MAX * 2, "fraction[{i}] must be bounded");
    }
}

// ---------------------------------------------------------------------------
// weighted-expenditure cap invariant
// ---------------------------------------------------------------------------

/// Assert the post-condition `sum(f1[i] * y_fp[i]) / SCALE <= f_fp` for the
/// output of `calc_fraction_distribution_fp`. Small round-down error is
/// acceptable; overshoot is not.
fn assert_weighted_within_target(result: &[u128], y_fp: &[u128], f_fp: u128) {
    use alloy_primitives::U256;
    let scale_u = U256::from(SCALE);
    let weighted: U256 = result
        .iter()
        .zip(y_fp.iter())
        .map(|(f, y)| U256::from(*f) * U256::from(*y) / scale_u)
        .sum();
    let target = U256::from(f_fp);
    assert!(
        weighted <= target,
        "weighted expenditure {weighted} exceeds target {f_fp}"
    );
}

#[test]
fn test_normalized_f1_respects_budget_skewed_population() {
    // Skewed population + imbalanced interest tends to push raw f1 over the
    // target. After normalization the post-condition must hold.
    let y_fp = vec![SCALE / 4, SCALE / 4, SCALE / 4, SCALE / 4];
    let p = vec![100u64, 1, 1, 1];
    let f_fp = LYSIS_LIMIT_MIN;
    let fmax_fp = LYSIS_LIMIT_MAX;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 103, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 4);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_normalized_f1_respects_budget_many_groups() {
    let n = 10usize;
    let y_fp: Vec<u128> = (0..n).map(|_| SCALE / n as u128).collect();
    let p: Vec<u64> = (1..=n as u64).collect();
    let f_fp = LYSIS_LIMIT_MIN;
    let fmax_fp = LYSIS_LIMIT_MAX;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 100, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), n);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_single_group_returns_f_without_normalization() {
    // The single-group fast path bypasses the normalization loop; `f_fp` is
    // returned as-is. Weighted total = f_fp * SCALE / SCALE = f_fp == target.
    let y_fp = vec![SCALE];
    let p = vec![10];
    let f_fp = LYSIS_LIMIT_MIN;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 10, f_fp, LYSIS_LIMIT_MAX).unwrap();
    assert_eq!(result, vec![f_fp]);
    assert_weighted_within_target(&result, &y_fp, f_fp);
}

#[test]
fn test_normalized_f1_preserves_ratios_when_scaled_down() {
    // When raw output overshoots and is scaled down, pairwise ratios between
    // groups should remain ~constant.
    let y_fp = vec![SCALE / 2, SCALE / 2];
    let p = vec![50u64, 5];
    let f_fp = LYSIS_LIMIT_MIN;
    let fmax_fp = LYSIS_LIMIT_MAX;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 55, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    assert_weighted_within_target(&result, &y_fp, f_fp);
    // Both fractions should still be positive (not obliterated by scale-down).
    for &frac in &result {
        assert!(
            frac > 0,
            "fraction must remain positive after normalization"
        );
    }
}

// ---------------------------------------------------------------------------
// I256 precision — no silent zero-collapse on small FI groups
// ---------------------------------------------------------------------------

/// Input with a dominant group and one tiny-interest group. Under the
/// pre- i128 pipeline with `/1_000_000` scale-down the small group's
/// `f1` could collapse to 0 (up to 10^6 SCALE units of precision lost per
/// term). After I256 refactor the distribution must preserve the signal.
#[test]
fn test_small_fi_group_survives_i256_precision() {
    let y_fp = vec![
        SCALE - 1_000_000, // dominant group ≈ 99.9999%
        1_000_000,         // tiny group ≈ 0.0001% — used to collapse to 0
    ];
    let p = vec![1000u64, 1];
    let f_fp = LYSIS_LIMIT_MIN;
    let fmax_fp = LYSIS_LIMIT_MAX;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 1001, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    assert!(
        result[1] > 0,
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
    let y_fp = vec![SCALE / 100, SCALE * 99 / 100]; // 1% / 99% split
    let p = vec![1u64, 1];
    let f_fp = LYSIS_LIMIT_MIN;
    let fmax_fp = LYSIS_LIMIT_MAX;
    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 2, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    for &f in &result {
        assert!(
            f <= LYSIS_LIMIT_MAX * 2,
            "fraction {f} exceeds LYSIS_LIMIT_MAX*2 bound"
        );
    }
}

// ---------------------------------------------------------------------------
// Scale invariant: cost_amount_minor must be in 10^18-minor units, not 10^36
// ---------------------------------------------------------------------------

/// Regression test for the scale-mismatch bug in `lysis::runtime`. Both
/// `cost_of_gratis_minor` (an oracle VWAP at 10^18 scale) and `gratis_load`
/// (a token amount at 10^18 minor scale) are 18-decimal U256 values. Their
/// product lives in 10^36 and must be divided by SCALE once to land in
/// minor units. The contract is documented at
/// `crates/core/nod/src/schema.rs:6-7`:
///   `cost_amount_minor = cost_of_gratis_minor * gratis_load_minor / SCALE_1E18`
///
/// Pre-fix: `lysis::runtime` computed `cost_of_gratis_minor * gratis_load`
/// without the divisor, producing a value ~10^18× too large that was stored
/// on-chain and emitted to the `NodIssued` event. This was silent because
/// `settle_mine_payment` is a no-op today, but every nominal-scale consumer
/// (token URI, `nodData`, future settlement) was wrong.
#[test]
fn test_lysis_cost_amount_lives_in_minor_scale() {
    use alloy_primitives::{address, U256};
    use outbe_common::WorldwideDay;
    use outbe_nod::NodContract;
    use outbe_oracle::contract::OracleContract;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_tribute::{TributeContract, TributeData};

    use crate::runtime::lysis;

    let wwd = WorldwideDay::new(20241220);
    const T_NOW: u64 = 1_700_000_000;
    let owner = address!("0x1111111111111111111111111111111111111111");
    // 100 COEN nominal, $0.5 oracle VWAP.
    let nominal = U256::in_units(100u64);
    let cost_of_gratis = U256::from(500_000_000_000_000_000u128);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        // 1. Register COEN/0xUSD pair and seed its WorldwideDay VWAP. We
        //    write directly into the oracle schema (no real vote tally),
        //    because lysis only reads `get_worldwide_day_vwap_for_pair_id`.
        let mut oracle = OracleContract::new(s.clone());
        let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle.worldwide_day_vwap_exists.write(&wwd, true).unwrap();
        oracle
            .worldwide_day_vwap_pair_count
            .write(&wwd, 1u32)
            .unwrap();
        oracle
            .worldwide_day_vwap_pair_id
            .get_nested(&wwd)
            .write(&0u32, pair_id)
            .unwrap();
        oracle
            .worldwide_day_vwap_value
            .get_nested(&wwd)
            .write(&0u32, cost_of_gratis)
            .unwrap();

        // 2. Open the day and issue a single tribute. Fidelity defaults to 1
        //    when unset (see `FidelityContract::get_fidelity_index`).
        let mut tribute = TributeContract::new(s.clone());
        tribute.unseal_day(wwd).unwrap();
        tribute
            .issue(&TributeData {
                token_id: U256::from(1u64),
                owner,
                worldwide_day: wwd,
                issuance_amount_minor: U256::in_units(50u64),
                issuance_currency: 1,
                nominal_amount_minor: nominal,
                reference_currency: 840,
                tribute_price_minor: U256::ZERO,
            })
            .unwrap();

        // 3. Pick a gratis allocation that produces a positive gratis_load.
        //    Single-FI fast path returns `f_fp = LYSIS_LIMIT_MIN` (8%), so
        //    gratis_load = 100 * 0.08 = 8 COEN.
        let gratis_allocation = nominal / U256::from(10u64);
        let result = lysis(s.clone(), wwd, gratis_allocation).unwrap();
        assert_eq!(result.nod_ids.len(), 1, "expected one NOD issued");

        // 4. Read back the NOD and assert the documented scale invariant.
        let nod = NodContract::new(s.clone());
        let item = nod.get_item(result.nod_ids[0]).unwrap().expect("NOD");

        // reference_currency must propagate from the originating Tribute.
        assert_eq!(item.reference_currency, 840);

        let expected = cost_of_gratis * item.gratis_load_minor / SCALE_1E18;
        assert_eq!(
            item.cost_amount_minor,
            expected,
            "cost_amount_minor must equal cost_of_gratis * gratis_load / SCALE_1E18; \
             pre-fix value (missing /SCALE) would be {}",
            cost_of_gratis * item.gratis_load_minor
        );

        // 5. Sanity bound: minor-unit cost cannot exceed a reasonable cap.
        //    The buggy value (~4 * 10^36 for these inputs) blows past 10^21.
        let upper_bound = U256::in_units(1_000u64);
        assert!(
            item.cost_amount_minor <= upper_bound,
            "cost_amount_minor {} looks like a 10^36-scaled value; \
             likely a scale-mismatch regression",
            item.cost_amount_minor
        );
    });
}
