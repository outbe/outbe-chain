//! Price markups and discounts in percentage points.

use alloy_primitives::U256;

/// Markup rates are percentage points over this denominator.
pub const PRICE_RATE_DEN: u16 = 100;

/// `base x (PRICE_RATE_DEN + rate_pct) / PRICE_RATE_DEN`, rounded down. `None` on overflow.
pub fn marked_up(base: U256, rate_pct: u16) -> Option<U256> {
    base.checked_mul(U256::from(u32::from(PRICE_RATE_DEN) + u32::from(rate_pct)))
        .map(|scaled| scaled / U256::from(PRICE_RATE_DEN))
}

/// `base` less `floor(base x rate_pct / PRICE_RATE_DEN)`, so the price rounds up.
/// `None` when the rate exceeds `PRICE_RATE_DEN`.
pub fn marked_down(base: U256, rate_pct: u16) -> Option<U256> {
    if rate_pct > PRICE_RATE_DEN {
        return None;
    }
    let discount = base.checked_mul(U256::from(rate_pct))? / U256::from(PRICE_RATE_DEN);
    Some(base - discount)
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

    #[test]
    fn marks_down_in_percentage_points_and_rounds_the_price_up() {
        let down = |base: u64, rate: u16| marked_down(U256::from(base), rate);
        assert_eq!(down(100, 4), Some(U256::from(96u64)));
        assert_eq!(down(26, 4), Some(U256::from(25u64)));
        assert_eq!(down(25, 4), Some(U256::from(24u64)));
        assert_eq!(down(1, 4), Some(U256::ONE));
        assert_eq!(down(1_000_000, 100), Some(U256::ZERO));
        assert_eq!(down(1_000_000, 101), None);
        assert_eq!(marked_down(U256::MAX, 4), None);
    }
}
