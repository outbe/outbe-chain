//! Price markup the rights derive their floor and call prices with.

use alloy_primitives::U256;

/// Markup rates are percentage points over this denominator.
pub const PRICE_RATE_DEN: u16 = 100;

/// `base x (PRICE_RATE_DEN + rate_pct) / PRICE_RATE_DEN`, rounded down. `None` on overflow.
pub fn marked_up(base: U256, rate_pct: u16) -> Option<U256> {
    base.checked_mul(U256::from(u32::from(PRICE_RATE_DEN) + u32::from(rate_pct)))
        .map(|scaled| scaled / U256::from(PRICE_RATE_DEN))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_up_in_percentage_points_and_rounds_down() {
        assert_eq!(marked_up(U256::from(100u64), 128), Some(U256::from(228u64)));
        assert_eq!(marked_up(U256::from(999u64), 8), Some(U256::from(1_078u64)));
        assert_eq!(
            marked_up(U256::from(7u64), u16::MAX),
            Some(U256::from(4_594u64))
        );
        assert_eq!(marked_up(U256::MAX, 1), None);
    }
}
