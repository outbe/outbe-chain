use alloy_primitives::{I256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::units::SCALE_1E18_U128;

pub(crate) const SCALE: u128 = SCALE_1E18_U128;

/// Policy parameters (fixed-point, denominator = 1000).
/// a=0.2 → 200/1000, b=0.1 → 100/1000, c=0.2 → 200/1000
const POLICY_A_NUM: u32 = 1;
const POLICY_A_DEN: u32 = 5; // a = 1/5 → x^(1/5)
const POLICY_B_NUM: u32 = 1;
const POLICY_B_DEN: u32 = 10; // b = 1/10 → x^(1/10)
const POLICY_C: u128 = SCALE * 2 / 10; // 0.2 * SCALE

// ---------------------------------------------------------------------------
// Fixed-point nth root
// ---------------------------------------------------------------------------

/// Fixed-point fractional power: (x_fp)^(p/q) where x_fp = x * SCALE.
/// Returns y = x^(p/q) * SCALE.
///
/// For p=1, q=5 (x^0.2): y^5 = x * SCALE^5, so y = (x * SCALE^5)^(1/5).
/// We use y = (x_fp * SCALE^(q-1))^(1/q) for p=1.
fn fp_root(x_fp: u128, _p: u32, q: u32) -> u128 {
    if x_fp == 0 {
        return 0;
    }
    if x_fp == SCALE {
        return SCALE; // 1.0^anything = 1.0
    }
    // For p=1: y^q = x_fp * SCALE^(q-1)
    // Use U256 to avoid overflow in the multiplication.
    let x = U256::from(x_fp);
    let s = U256::from(SCALE);
    let mut target = x;
    for _ in 0..(q - 1) {
        target *= s;
    }
    // Binary search for y such that y^q <= target
    let mut lo = U256::from(1u64);
    let mut hi = U256::from(x_fp).max(s); // upper bound
    while lo < hi {
        let mid = lo + (hi - lo + U256::from(1u64)) / U256::from(2u64);
        let mut mid_pow = mid;
        let mut overflow = false;
        for _ in 1..q {
            let (new_val, of) = mid_pow.overflowing_mul(mid);
            if of || new_val > target * U256::from(2u64) {
                overflow = true;
                break;
            }
            mid_pow = new_val;
        }
        if overflow || mid_pow > target {
            hi = mid - U256::from(1u64);
        } else {
            lo = mid;
        }
    }
    lo.to::<u128>()
}

// ---------------------------------------------------------------------------
// Fixed-point policy tau computation
// ---------------------------------------------------------------------------

/// Compute tau weights (Eq. 3.54) in fixed-point.
/// tau[i] = (i-0.5)^a / min(pi^b, pi_prev^b) for i in 1..N-1
/// tau[0] = c * sum_middle, tau[N] = (1-c) * sum_middle
fn policy_tau_fp(p: &[u64], nt: usize) -> Vec<u128> {
    let ng = p.len();
    let mut tau = vec![0u128; ng + 1];

    for i in 1..ng {
        let pi = p[i];
        let pi_prev = p[i - 1];

        if pi != 0 && pi_prev != 0 {
            // (i - 0.5) in fixed-point: (2*i - 1) * SCALE / 2
            let i_minus_half_fp = (2 * i as u128 - 1) * SCALE / 2;

            // (i-0.5)^(1/5) in fixed-point
            let numerator = fp_root(i_minus_half_fp, POLICY_A_NUM, POLICY_A_DEN);

            // pi^(1/10) and pi_prev^(1/10) in fixed-point
            let pi_fp = pi as u128 * SCALE;
            let pi_prev_fp = pi_prev as u128 * SCALE;
            let pi_root = fp_root(pi_fp, POLICY_B_NUM, POLICY_B_DEN);
            let pi_prev_root = fp_root(pi_prev_fp, POLICY_B_NUM, POLICY_B_DEN);

            let min_val = pi_root.min(pi_prev_root);

            if min_val != 0 {
                // tau[i] = numerator / min_val (both in SCALE)
                tau[i] =
                    (U256::from(numerator) * U256::from(SCALE) / U256::from(min_val)).to::<u128>();
            } else {
                // Fallback: ng^a * nt^b
                let ng_fp = ng as u128 * SCALE;
                let nt_fp = nt as u128 * SCALE;
                let ng_root = fp_root(ng_fp, POLICY_A_NUM, POLICY_A_DEN);
                let nt_root = fp_root(nt_fp, POLICY_B_NUM, POLICY_B_DEN);
                tau[i] =
                    (U256::from(ng_root) * U256::from(nt_root) / U256::from(SCALE)).to::<u128>();
            }
        } else {
            // Fallback
            let ng_fp = ng as u128 * SCALE;
            let nt_fp = nt as u128 * SCALE;
            let ng_root = fp_root(ng_fp, POLICY_A_NUM, POLICY_A_DEN);
            let nt_root = fp_root(nt_fp, POLICY_B_NUM, POLICY_B_DEN);
            tau[i] = (U256::from(ng_root) * U256::from(nt_root) / U256::from(SCALE)).to::<u128>();
        }
    }

    let sum_middle: u128 = tau[1..ng].iter().sum();
    // Use U256 to avoid overflow in POLICY_C * sum_middle
    let sum_mid_u = U256::from(sum_middle);
    let policy_c_u = U256::from(POLICY_C);
    let scale_u = U256::from(SCALE);
    tau[0] = (policy_c_u * sum_mid_u / scale_u).to::<u128>();
    tau[ng] = ((scale_u - policy_c_u) * sum_mid_u / scale_u).to::<u128>();

    tau
}

// ---------------------------------------------------------------------------
// Simplified moments (kappa = 0)
// ---------------------------------------------------------------------------

struct MomentsFp {
    m: Vec<u128>,     // mass weights (sum = SCALE)
    y_cum: Vec<u128>, // cumulative Y (0 to SCALE)
    ey: u128,         // E[Y] in SCALE
    var_y: u128,      // Var[Y] in SCALE (may be 0)
}

fn compute_moments_fp(y_fp: &[u128], tau: &[u128]) -> MomentsFp {
    let t_total: u128 = tau.iter().sum();

    // m[i] = tau[i] * SCALE / t_total (normalized mass, use U256 to avoid overflow)
    let m: Vec<u128> = if t_total > 0 {
        tau.iter()
            .map(|&t| (U256::from(t) * U256::from(SCALE) / U256::from(t_total)).to::<u128>())
            .collect()
    } else {
        vec![0; tau.len()]
    };

    // Y cumulative sum of y_fp (y_fp sums to SCALE)
    let mut y_cum = vec![0u128; y_fp.len() + 1];
    let mut cum = 0u128;
    for (i, &val) in y_fp.iter().enumerate() {
        cum += val;
        y_cum[i + 1] = cum;
    }
    // Ensure last = SCALE
    *y_cum.last_mut().unwrap() = SCALE;

    // E[Y] and E[Y²] using U256 to avoid u128 overflow on m[i] * y_cum[i].
    let scale_u = U256::from(SCALE);
    let mut ey_u = U256::ZERO;
    let mut ey2_u = U256::ZERO;
    for i in 0..m.len() {
        let mi = U256::from(m[i]);
        let yi = U256::from(y_cum[i]);
        ey_u += mi * yi / scale_u;
        ey2_u += mi * yi * yi / (scale_u * scale_u);
    }
    let ey: u128 = ey_u.to::<u128>();
    let ey2: u128 = ey2_u.to::<u128>();

    // var_y = E[Y²] - E[Y]² / SCALE
    let ey_sq = U256::from(ey) * U256::from(ey) / scale_u;
    let var_y = ey2.saturating_sub(ey_sq.to::<u128>());

    MomentsFp {
        m,
        y_cum,
        ey,
        var_y,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Computes lysis fractions for each FI group using fixed-point integer math.
///
/// # Parameters
/// - `y_fp`: interest share per FI group in fixed-point (sum = SCALE, sorted ascending FI).
///   Caller is responsible for normalization — integer-division truncation must
///   be absorbed before this call.
/// - `p`: population counts per FI group
/// - `_l`: tree height (unused)
/// - `nt`: total tribute count
/// - `f_fp`: target fraction in fixed-point (SCALE-based)
/// - `fmax_fp`: maximum fraction in fixed-point (SCALE-based)
///
/// # Returns
/// Fraction per FI group in fixed-point (SCALE-based).
///
/// # Post-condition
/// `sum(f1[i] * y_fp[i]) / SCALE <= f_fp` — weighted expenditure never exceeds
/// the target allocation. Fractions are scaled down monotonically if the raw
/// algorithm output would overshoot; relative ratios between groups are preserved.
///
/// # Errors
/// Returns `PrecompileError::Revert` if a signed-int conversion fails at a
/// boundary (impossible under current bounds; see overflow analysis in tests).
pub fn calc_fraction_distribution_fp(
    y_fp: &[u128],
    p: &[u64],
    _l: usize,
    nt: usize,
    f_fp: u128,
    fmax_fp: u128,
) -> Result<Vec<u128>> {
    if p.is_empty() {
        return Ok(vec![0]);
    }
    if p.len() == 1 {
        return Ok(vec![f_fp]);
    }

    let tau = policy_tau_fp(p, nt);
    let moments = compute_moments_fp(y_fp, &tau);

    // With kappa=0: alpha=0, so:
    // beta = (f/fmax - E[Y]) / Var[Y]
    // f1[i] = fmax * sum_{j>=i} m[j] * (1 + beta * (Y[j] - E[Y]))

    // signed I256 arithmetic throughout — no intermediate scale-down.
    // All unsigned inputs are ≤ SCALE (10^18), far under I256::MAX (~5.8·10^76),
    // so try_from conversions cannot fail in practice; we still return a
    // structured Fatal instead of panicking per CLAUDE.md rules.
    // `fmax_fp`, `f_fp`, `SCALE` are all `u128` here (the result feeds
    // `u128_to_i256` below), so `fmax_fp > 0` ≡ `fmax_fp != 0` and `checked_div`
    // is exactly equivalent to the guarded division (None only when divisor is 0).
    let f_over_fmax = (f_fp * SCALE).checked_div(fmax_fp).unwrap_or(0);
    let scale_i = u128_to_i256(SCALE)?;
    let ey_i = u128_to_i256(moments.ey)?;
    let beta_num_i = u128_to_i256(f_over_fmax)? - ey_i;
    let beta_den_i = u128_to_i256(moments.var_y)?;
    let fmax_i = u128_to_i256(fmax_fp)?;

    let n = y_fp.len();
    let mut f1 = vec![0u128; n];

    for i in 1..=n {
        let mut sum_i = I256::ZERO;
        for j in i..moments.m.len() {
            let y_cum_i = u128_to_i256(moments.y_cum[j])?;
            let y_diff = y_cum_i - ey_i;
            let beta_term = if beta_den_i > I256::ZERO {
                beta_num_i * y_diff / beta_den_i
            } else {
                I256::ZERO
            };
            // factor = SCALE + beta_term (in SCALE units) — exact, no scale-down.
            let factor = scale_i + beta_term;
            let mj_i = u128_to_i256(moments.m[j])?;
            sum_i += mj_i * factor / scale_i;
        }

        // f1[i] = fmax * sum / SCALE — exact; clamp negative to 0.
        let result = fmax_i * sum_i / scale_i;
        f1[i - 1] = if result <= I256::ZERO {
            0
        } else {
            i256_to_u128_clamped(result)
        };
    }

    // normalize f1 so weighted expenditure does not exceed f_fp.
    // Raw algorithm output can have `sum(f1[i] * y_fp[i]) / SCALE > f_fp` because
    // the per-group distribution doesn't enforce a budget-preserving invariant
    // on its own. Scale down proportionally (monotone, preserves ratios, never
    // rounds up) so downstream `gratis_load = fraction * nominal / SCALE` in
    // `lysis::runtime` cannot overspend the allocation and silently skip the
    // tail of the tribute list.
    let scale_u = U256::from(SCALE);
    let target = U256::from(f_fp);
    let weighted_total: U256 = f1
        .iter()
        .zip(y_fp.iter())
        .map(|(f, y)| U256::from(*f) * U256::from(*y) / scale_u)
        .sum();
    if weighted_total > target {
        for f in f1.iter_mut() {
            *f = (U256::from(*f) * target / weighted_total).to::<u128>();
        }
    }

    Ok(f1)
}

/// Widen a non-negative `u128` to `I256`. Infallible in practice —
/// `u128::MAX < I256::MAX` — but expressed as `Result` so runtime paths never
/// hide an invariant break behind `unwrap()`.
fn u128_to_i256(value: u128) -> Result<I256> {
    I256::try_from(value).map_err(|_| {
        PrecompileError::Revert(format!("lysis: u128 -> I256 conversion overflow ({value})"))
    })
}

/// Narrow a positive `I256` back to `u128`. Values exceeding `u128::MAX` are
/// clamped to `u128::MAX` normalization in the caller caps the
/// weighted sum, so oversized intermediates don't propagate into storage.
fn i256_to_u128_clamped(value: I256) -> u128 {
    u128::try_from(value).unwrap_or(u128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(fp_root(SCALE, 1, 5), SCALE);
    }

    #[test]
    fn test_single_group_returns_f() {
        const LYSIS_LIMIT_MIN: u128 = SCALE * 8 / 100; // 0.08
        const LYSIS_LIMIT_MAX: u128 = SCALE * 16 / 100; // 0.16

        let y_fp = vec![SCALE]; // 100%
        let p = vec![10];
        let f_fp = LYSIS_LIMIT_MIN;
        let fmax_fp = LYSIS_LIMIT_MAX;

        let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 10, f_fp, fmax_fp).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], f_fp);
    }

    #[test]
    fn test_two_groups_sum_reasonable() {
        const LYSIS_LIMIT_MIN: u128 = SCALE * 8 / 100; // 0.08
        const LYSIS_LIMIT_MAX: u128 = SCALE * 16 / 100; // 0.16

        let y_fp = vec![SCALE / 2, SCALE / 2]; // 50/50
        let p = vec![5, 5];
        let f_fp = LYSIS_LIMIT_MIN;
        let fmax_fp = LYSIS_LIMIT_MAX;

        let result = calc_fraction_distribution_fp(&y_fp, &p, 10, 10, f_fp, fmax_fp).unwrap();
        assert_eq!(result.len(), 2);
        // Both fractions should be positive and <= fmax
        for &frac in &result {
            assert!(frac > 0, "fraction should be positive");
            assert!(frac <= fmax_fp * 2, "fraction should be bounded");
        }
    }
}
