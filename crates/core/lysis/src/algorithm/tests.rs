use super::*;

/// The symbolic share as a day-average fraction, and the per-league ceiling twice that.
/// Production derives both from the day; these only feed the tests below.
const F_FP_DEFAULT: U256 = u256_from_u128(SCALE_U128 * 32 / 100);
const F_MAX_FP: U256 = u256_from_u128(SCALE_U128 * 64 / 100);

/// Compute floor(base^exp) for u128, saturating on overflow.
fn pow_u128(base: u128, exp: u32) -> Option<u128> {
    let mut result: u128 = 1;
    for _ in 0..exp {
        result = result.checked_mul(base)?;
    }
    Some(result)
}

/// Integer nth root: floor(x^(1/n)) via binary search.
fn int_nth_root(x: u128, n: u32) -> u128 {
    if x <= 1 || n == 1 {
        return x;
    }
    let bit_bound = 128 / n;
    let mut hi: u128 = if bit_bound >= 64 {
        x
    } else {
        (1u128 << (bit_bound + 1)).min(x)
    };
    let mut lo: u128 = 1;
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        match pow_u128(mid, n) {
            Some(v) if v <= x => lo = mid,
            _ => hi = mid - 1,
        }
    }
    lo
}

#[test]
fn test_int_nth_root() {
    assert_eq!(int_nth_root(32, 5), 2); // 2^5 = 32
    assert_eq!(int_nth_root(100, 2), 10); // 10^2 = 100
    assert_eq!(int_nth_root(1000, 3), 10); // 10^3 = 1000
    assert_eq!(int_nth_root(1, 5), 1);
    assert_eq!(int_nth_root(0, 5), 0);
}

#[test]
fn test_fp_root_identity() {
    // 1.0^(1/5) = 1.0
    assert_eq!(fp_root(SCALE, 1, 5).unwrap(), SCALE);
}

/// Reference `round(base^(1/q) * SCALE)` computed in `f64` (test-only per
/// CLAUDE.md section 5.6). Used only to know the *magnitude* of the expected root.
/// [`test_fp_root_floor_identity`] pins the exactness in integers.
fn ref_root_scaled(base: u128, q: u32) -> u128 {
    let root = (base as f64).powf(1.0 / q as f64);
    (root * SCALE_U128 as f64).round() as u128
}

/// Known-answer test: `fp_root` must land within a tiny tolerance of the
/// real fractional power at the in-use `q in {5, 10}`. A scale-stacking wrap
/// (the OIP-00043 bug) makes `target` - and thus the root - garbage, off by
/// many orders of magnitude. Such a root fails this check. It still satisfies
/// the loose `frac > 0` / `frac <= 2*fmax` bounds the distribution tests use.
#[test]
fn test_fp_root_known_answers() {
    // pi^(1/10) at q = 10 - the call site that overflows U256 hardest.
    for &pi in &[2u128, 16, 1000, 1_000_000] {
        let got: u128 = fp_root(U256::from(pi) * SCALE, POLICY_B_NUM, POLICY_B_DEN)
            .unwrap()
            .try_into()
            .unwrap();
        let want = ref_root_scaled(pi, 10);
        let diff = got.abs_diff(want);
        // fp_root returns the integer floor of the true root; allow a few
        // ULP for f64 rounding in the reference. A wrap is off by >> this.
        assert!(
            diff <= 4_096,
            "fp_root({pi}*SCALE, 1, 10) = {got}, expected ~{want} (diff {diff})"
        );
    }

    // (i-0.5)^(1/5) at q = 5 for representative x_fp = x * SCALE.
    for &(num, den) in &[(1u128, 2u128), (3, 2), (5, 2), (99, 2)] {
        let x_fp = U256::from(num) * SCALE / U256::from(den);
        let got: u128 = fp_root(x_fp, POLICY_A_NUM, POLICY_A_DEN)
            .unwrap()
            .try_into()
            .unwrap();
        // reference: (num/den)^(1/5) * SCALE
        let root = ((num as f64) / (den as f64)).powf(1.0 / 5.0);
        let want = (root * SCALE_U128 as f64).round() as u128;
        let diff = got.abs_diff(want);
        assert!(
            diff <= 4_096,
            "fp_root({num}/{den}*SCALE, 1, 5) = {got}, expected ~{want} (diff {diff})"
        );
    }
}

/// Exact integer defining-identity: `fp_root` returns `floor(target^(1/q))`
/// where `target = x_fp * SCALE^(q-1)`. Verified as `y^q <= target < (y+1)^q`,
/// with `target` and the powers computed in the wide reference type so a
/// U256 wrap cannot slip through. No `f64`; fully deterministic.
#[test]
fn test_fp_root_floor_identity() {
    let scale_w = U1024::from(SCALE);
    for &q in &[POLICY_A_DEN, POLICY_B_DEN] {
        for &pi in &[2u128, 7, 16, 255, 1000, 1_000_000] {
            let x_fp = U256::from(pi) * SCALE;
            let y = fp_root(x_fp, 1, q).unwrap();

            // target = x_fp * SCALE^(q-1), exact in U1024.
            let mut target = U1024::from(x_fp);
            for _ in 0..(q - 1) {
                target = target.checked_mul(scale_w).unwrap();
            }

            let yw = U1024::from(y);
            let y_pow = pow_u1024(yw, q);
            let y1_pow = pow_u1024(yw + U1024::from(1u64), q);
            assert!(
                y_pow <= target && target < y1_pow,
                "fp_root({pi}*SCALE,1,{q})={y}: floor identity broken"
            );
        }
    }
}

/// The largest in-use `x_fp` (a fidelity-group population up to the
/// OIP-00043 `pi <= 10^9` bound, at the hardest `q = 10`) must return `Ok`
/// with a correctly-bounded root - not a revert and not a wrap.
#[test]
fn test_fp_root_no_overflow_at_q10() {
    let pi: u128 = 1_000_000_000; // 10^9
    let root = fp_root(U256::from(pi) * SCALE, POLICY_B_NUM, POLICY_B_DEN)
        .expect("fp_root must not overflow for in-use populations");
    // 10^9^(1/10) = 10^0.9 ~= 7.94, so root ~= 7.94 * SCALE.
    let want = ref_root_scaled(pi, 10);
    let got: u128 = root.try_into().unwrap();
    assert!(
        got.abs_diff(want) <= 4_096,
        "fp_root(10^9*SCALE,1,10) = {got}, expected ~{want}"
    );
}

/// `y^exp` in U1024, saturating at `U1024::MAX` (only hit for out-of-range
/// inputs this test never supplies).
fn pow_u1024(base: U1024, exp: u32) -> U1024 {
    let mut acc = U1024::from(1u64);
    for _ in 0..exp {
        acc = acc.saturating_mul(base);
    }
    acc
}

#[test]
fn test_single_group_returns_f() {
    let y_fp = vec![SCALE]; // 100%
    let p = vec![10];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;

    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], f_fp);
}

#[test]
fn test_two_groups_sum_reasonable() {
    let half = SCALE / U256::from(2u64);
    let y_fp = vec![half, half]; // 50/50
    let p = vec![5, 5];
    let f_fp = F_FP_DEFAULT;
    let fmax_fp = F_MAX_FP;

    let result = calc_fraction_distribution_fp(&y_fp, &p, 10, f_fp, fmax_fp).unwrap();
    assert_eq!(result.len(), 2);
    // Both fractions should be positive and <= fmax
    for &frac in &result {
        assert!(!frac.is_zero(), "fraction should be positive");
        assert!(
            frac <= fmax_fp * U256::from(2u64),
            "fraction should be bounded"
        );
    }
}
