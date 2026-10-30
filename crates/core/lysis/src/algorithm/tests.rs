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
/// the loose `frac > 0` / `frac <= fmax` bounds the distribution tests use.
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
        assert!(frac <= fmax_fp, "fraction should be bounded by fmax_fp");
    }
}

#[test]
fn prop_fraction_never_exceeds_ceiling() {
    // 1. Exact T-03 multi-group counterexample: raw f1[0] = 670_117 > fmax_fp = 640_000,
    // while weighted_total = 639_999 <= f_fp = 640_000 (normalization does not fire).
    let capped = calc_fraction_distribution_fp(
        &[U256::from(200_000u64), U256::from(800_000u64)],
        &[1, 1],
        2,
        F_MAX_FP,
        F_MAX_FP,
    )
    .unwrap();
    assert_eq!(capped, vec![U256::from(640_000u64), U256::from(632_470u64)]);

    // 2. Singleton fast-path when f_fp > fmax_fp and when fmax_fp == 0.
    let single_capped =
        calc_fraction_distribution_fp(&[SCALE], &[1], 1, F_MAX_FP, F_FP_DEFAULT).unwrap();
    assert_eq!(single_capped, vec![F_FP_DEFAULT]);
    let single_zero_fmax =
        calc_fraction_distribution_fp(&[SCALE], &[1], 1, F_FP_DEFAULT, U256::ZERO).unwrap();
    assert_eq!(single_zero_fmax, vec![U256::ZERO]);

    // 3. Multi-group zero paths: fmax_fp == 0 and t_total == 0 (all populations 0, nt == 0).
    let half = SCALE / U256::from(2u64);
    let multi_zero_fmax =
        calc_fraction_distribution_fp(&[half, half], &[1, 1], 2, F_FP_DEFAULT, U256::ZERO).unwrap();
    assert_eq!(multi_zero_fmax, vec![U256::ZERO, U256::ZERO]);

    let zero_total_tau =
        calc_fraction_distribution_fp(&[half, half], &[0, 0], 0, F_FP_DEFAULT, F_MAX_FP).unwrap();
    assert_eq!(zero_total_tau, vec![U256::ZERO, U256::ZERO]);

    // 4. Grid over group counts, share distributions (including degenerate Var[Y] and tiny shares),
    // populations, and (f_fp, fmax_fp) ratios (including f == fmax and f > fmax).
    let share_cases: &[&[u64]] = &[
        &[500_000, 500_000],
        &[200_000, 800_000],
        &[10_000, 990_000],
        &[1, 999_999],
        &[999_999, 1],
        &[100_000, 200_000, 700_000],
        &[50_000, 150_000, 300_000, 500_000],
        &[0, 0, 1_000_000],
    ];
    let pop_cases: &[&[u64]] = &[&[1, 1], &[1, 1000], &[1000, 1], &[500_000_000, 500_000_000]];
    let limit_cases: &[(U256, U256)] = &[
        (F_FP_DEFAULT, F_MAX_FP),
        (F_MAX_FP, F_MAX_FP),
        (F_MAX_FP, F_FP_DEFAULT),
        (SCALE, F_MAX_FP),
    ];

    for &shares in share_cases {
        let y_fp: Vec<U256> = shares.iter().copied().map(U256::from).collect();
        let n = y_fp.len();
        for &pops_template in pop_cases {
            let p: Vec<u64> = (0..n)
                .map(|idx| pops_template[idx % pops_template.len()])
                .collect();
            let nt: usize = p.iter().copied().map(|v| v as usize).sum();
            for &(f_fp, fmax_fp) in limit_cases {
                let out = calc_fraction_distribution_fp(&y_fp, &p, nt, f_fp, fmax_fp).unwrap();
                assert_eq!(out.len(), n);
                for (idx, &frac) in out.iter().enumerate() {
                    assert!(
                        frac <= fmax_fp,
                        "frac[{idx}] = {frac} exceeds ceiling fmax_fp = {fmax_fp} for shares={shares:?}, p={p:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn golden_fmax_twice_f_outputs_are_unchanged() {
    // 1. Negative-beta case with non-zero signed division remainder.
    let neg_beta = calc_fraction_distribution_fp(
        &[U256::from(10_000u64), U256::from(990_000u64)],
        &[1, 1],
        2,
        F_FP_DEFAULT,
        F_MAX_FP,
    )
    .unwrap();
    assert_eq!(
        neg_beta,
        vec![U256::from(586_432u64), U256::from(317_308u64)]
    );

    // 2. Balanced 2-group case.
    let balanced_2 = calc_fraction_distribution_fp(
        &[U256::from(500_000u64), U256::from(500_000u64)],
        &[5, 5],
        10,
        F_FP_DEFAULT,
        F_MAX_FP,
    )
    .unwrap();
    assert_eq!(
        balanced_2,
        vec![U256::from(515_121u64), U256::from(124_878u64)]
    );

    // 3. Large-population 2-group boundary witness (p = [500_000_000, 500_000_000]).
    let large_pop_2 = calc_fraction_distribution_fp(
        &[U256::from(200_000u64), U256::from(800_000u64)],
        &[500_000_000, 500_000_000],
        1_000_000_000,
        F_FP_DEFAULT,
        F_MAX_FP,
    )
    .unwrap();
    assert_eq!(
        large_pop_2,
        vec![U256::from(575_999u64), U256::from(255_999u64)]
    );

    // 4. 3-group case.
    let groups_3 = calc_fraction_distribution_fp(
        &[
            U256::from(500_000u64),
            U256::from(300_000u64),
            U256::from(200_000u64),
        ],
        &[50, 30, 20],
        100,
        F_FP_DEFAULT,
        F_MAX_FP,
    )
    .unwrap();
    assert_eq!(
        groups_3,
        vec![
            U256::from(460_224u64),
            U256::from(240_437u64),
            U256::from(88_784u64),
        ]
    );

    // 5. 4-group skewed-population case.
    let q = SCALE / U256::from(4u64);
    let groups_4 =
        calc_fraction_distribution_fp(&[q, q, q, q], &[100, 1, 1, 1], 103, F_FP_DEFAULT, F_MAX_FP)
            .unwrap();
    assert_eq!(
        groups_4,
        vec![
            U256::from(517_793u64),
            U256::from(379_522u64),
            U256::from(244_974u64),
            U256::from(137_709u64),
        ]
    );
}

#[test]
fn kernel_checked_math_rejects_overflow_inputs() {
    use crate::errors::{LysisArithmeticErrorV1, ProgramErrorV1};
    use outbe_primitives::error::PrecompileError;

    // 1. Cumulative share sum overflow (`cum.checked_add` in `compute_moments_fp`).
    let err_cum =
        calc_fraction_distribution_fp(&[U256::MAX, U256::ONE], &[1, 1], 2, F_FP_DEFAULT, F_MAX_FP)
            .unwrap_err();
    assert!(
        matches!(&err_cum, PrecompileError::Revert(msg) if msg == "lysis: kernel arithmetic overflow"),
        "unexpected error for cumulative sum overflow: {err_cum:?}"
    );

    // 2. Scaled multiplication overflow (`checked_mul_div_floor(f_fp, SCALE, fmax_fp)`).
    let half = SCALE / U256::from(2u64);
    let err_mul_div =
        calc_fraction_distribution_fp(&[half, half], &[1, 1], 2, U256::MAX, SCALE).unwrap_err();
    assert!(
        matches!(&err_mul_div, PrecompileError::Revert(msg) if msg == "scaled_math: multiplication overflow"),
        "unexpected error for scaled_math overflow: {err_mul_div:?}"
    );

    // 3. Square overflow (`yi.checked_mul(yi)` in `compute_moments_fp` at `yi = 2^128`).
    let err_sq = calc_fraction_distribution_fp(
        &[U256::ONE << 128, U256::ZERO],
        &[1, 1],
        2,
        F_FP_DEFAULT,
        F_MAX_FP,
    )
    .unwrap_err();
    assert!(
        matches!(&err_sq, PrecompileError::Revert(msg) if msg == "lysis: kernel arithmetic overflow"),
        "unexpected error for yi^2 overflow: {err_sq:?}"
    );

    // 4. `program_v1` boundary preserves `ProgramErrorV1::Arithmetic { reason: Math(_) }`
    // and maps to `PrecompileError::BodyReadCorruption`.
    let prog_err = ProgramErrorV1::math(err_sq);
    assert!(matches!(
        &prog_err,
        ProgramErrorV1::Arithmetic {
            reason: LysisArithmeticErrorV1::Math(msg)
        } if msg.contains("lysis: kernel arithmetic overflow")
    ));
    let boundary_err: PrecompileError = prog_err.into();
    assert!(matches!(
        boundary_err,
        PrecompileError::BodyReadCorruption(_)
    ));
}
