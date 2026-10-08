//! Deterministic integer helpers for the source aggregation: the sigma-based
//! deviation filter and the wide-integer narrowing and square root it uses.

use crate::fixed::FixedValue;
use alloy_primitives::{aliases::U1024, U256, U512};
use eyre::Result;
use outbe_primitives::units::SCALE_1E18;

/// Filters prices that deviate more than `threshold` standard deviations from
/// the median. Prices, threshold, mean, variance and square root are all
/// deterministic integers. The threshold is dimensionless FP18.
pub(super) fn filter_deviations(
    prices: &[(FixedValue, FixedValue)],
    threshold: FixedValue,
) -> Result<Vec<(FixedValue, FixedValue)>> {
    if prices.len() <= 1 {
        return Ok(prices.to_vec());
    }

    let mut sorted: Vec<U256> = prices.iter().map(|(price, _)| price.raw()).collect();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];

    let sum = sorted.iter().try_fold(U512::ZERO, |sum, price| {
        sum.checked_add(U512::from(*price))
            .ok_or_else(|| eyre::eyre!("deviation mean sum overflow"))
    })?;
    let mean = narrow_u512(sum / U512::from(sorted.len()), "deviation mean")?;
    let sum_sq = sorted.iter().try_fold(U1024::ZERO, |sum, price| {
        let deviation = price.abs_diff(mean);
        let wide = U1024::from(deviation);
        sum.checked_add(wide * wide)
            .ok_or_else(|| eyre::eyre!("deviation square sum overflow"))
    })?;
    let variance = sum_sq / U1024::from(sorted.len());
    let std_dev = narrow_u1024(isqrt_u1024(variance), "deviation standard deviation")?;

    if std_dev.is_zero() {
        return Ok(prices.to_vec());
    }

    let allowed = U512::from(threshold.raw()) * U512::from(std_dev) / U512::from(SCALE_1E18);

    Ok(prices
        .iter()
        .filter(|(price, _)| U512::from(price.raw().abs_diff(median)) <= allowed)
        .cloned()
        .collect())
}

pub(super) fn narrow_u512(value: U512, label: &'static str) -> Result<U256> {
    if value > U512::from(U256::MAX) {
        return Err(eyre::eyre!("{label} exceeds U256"));
    }
    Ok(value.wrapping_to::<U256>())
}

pub(super) fn narrow_u1024(value: U1024, label: &'static str) -> Result<U256> {
    if value > U1024::from(U256::MAX) {
        return Err(eyre::eyre!("{label} exceeds U256"));
    }
    Ok(value.wrapping_to::<U256>())
}

pub(super) fn isqrt_u1024(n: U1024) -> U1024 {
    if n.is_zero() {
        return U1024::ZERO;
    }
    if n == U1024::ONE {
        return U1024::ONE;
    }
    let mut x = n;
    let mut y = (x >> 1) + (x & U1024::ONE);
    while y < x {
        x = y;
        y = (x + n / x) >> 1;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(value: &str) -> FixedValue {
        FixedValue::parse(value).unwrap()
    }

    #[test]
    fn test_filter_deviations() {
        let prices = vec![
            (fp("100"), fp("1")),
            (fp("101"), fp("1")),
            (fp("102"), fp("1")),
            (fp("999"), fp("1")),
        ];
        let filtered = filter_deviations(&prices, fp("2")).unwrap();
        assert!(filtered.len() < prices.len());
        assert!(filtered.iter().all(|(p, _)| *p < fp("500")));
    }

    #[test]
    fn test_filter_identical_prices() {
        let prices = vec![
            (fp("100"), fp("1")),
            (fp("100"), fp("1")),
            (fp("100"), fp("1")),
        ];
        let filtered = filter_deviations(&prices, fp("2")).unwrap();
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn deviation_filter_includes_the_boundary_and_excludes_one_minor_unit_beyond_it() {
        let one = FixedValue::from_raw(U256::ONE);
        let on_boundary = vec![
            (FixedValue::from_raw(U256::from(100u64)), one),
            (FixedValue::from_raw(U256::from(100u64)), one),
            (FixedValue::from_raw(U256::from(102u64)), one),
            (FixedValue::from_raw(U256::from(102u64)), one),
        ];
        assert_eq!(filter_deviations(&on_boundary, fp("2")).unwrap().len(), 4);

        let one_unit_outside = vec![
            (FixedValue::from_raw(U256::from(99u64)), one),
            (FixedValue::from_raw(U256::from(100u64)), one),
            (FixedValue::from_raw(U256::from(102u64)), one),
            (FixedValue::from_raw(U256::from(102u64)), one),
        ];
        let filtered = filter_deviations(&one_unit_outside, fp("2")).unwrap();

        assert_eq!(filtered.len(), 3);
        assert!(!filtered
            .iter()
            .any(|(price, _)| price.raw() == U256::from(99u64)));
    }
}
