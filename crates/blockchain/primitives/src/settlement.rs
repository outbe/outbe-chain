//! Settlement rules shared by the rights and by Lysis: the cost, computed at full
//! precision with any FX leg and floored once into the asset's minor units, the
//! settlement deadline, and who pays in which asset.

use alloy_primitives::{Address, U256};

pub use crate::errors::RoundingError;
use crate::units::{MAX_ASSET_DECIMALS, PROTOCOL_AMOUNT_DECIMALS, SCALE_1E6_U256};

/// Decimals of a price x load product.
pub const COST_DECIMALS: u8 = 2 * PROTOCOL_AMOUNT_DECIMALS;

/// `floor(numerator x 10^asset_decimals / (denominator x 10^cost_decimals))`.
///
/// `numerator / denominator` is the cost carrying `cost_decimals`.
/// A rate leg belongs in that fraction, so this function floors it with the unit
/// scaling.
pub fn floor_to_asset_units(
    numerator: U256,
    denominator: U256,
    cost_decimals: u8,
    asset_decimals: u8,
) -> Result<U256, RoundingError> {
    if asset_decimals > MAX_ASSET_DECIMALS {
        return Err(RoundingError::UnsupportedDecimals(asset_decimals));
    }
    if denominator.is_zero() {
        return Err(RoundingError::ZeroRate);
    }
    let asset_decimals = u32::from(asset_decimals);
    let cost_decimals = u32::from(cost_decimals);
    let (numerator, denominator) = if asset_decimals >= cost_decimals {
        let scaled = numerator
            .checked_mul(pow10(asset_decimals - cost_decimals))
            .ok_or(RoundingError::Overflow)?;
        (scaled, denominator)
    } else {
        let scaled = denominator
            .checked_mul(pow10(cost_decimals - asset_decimals))
            .ok_or(RoundingError::Overflow)?;
        (numerator, scaled)
    };
    let units = numerator / denominator;
    if units.is_zero() && !numerator.is_zero() {
        return Err(RoundingError::RoundsToZero);
    }
    Ok(units)
}

fn pow10(exponent: u32) -> U256 {
    U256::from(10u64).pow(U256::from(exponent))
}

/// What a settlement charges for: `entry x load x units`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementCost {
    pub entry_price_minor: U256,
    pub load_minor: U256,
    pub units: U256,
}

/// The cost in the asset's minor units, floored once. A positive cost is at
/// least one reference-currency minor unit. `rate` is `(COEN/issuance, COEN/reference)` on
/// the issuance rail.
pub fn settlement_cost_units(
    cost: &SettlementCost,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256, RoundingError> {
    let amount = cost
        .entry_price_minor
        .checked_mul(cost.load_minor)
        .and_then(|v| v.checked_mul(cost.units))
        .ok_or(RoundingError::Overflow)?;
    let amount = if amount.is_zero() {
        amount
    } else {
        amount.max(SCALE_1E6_U256)
    };
    let (numerator, denominator) = match rate {
        Some((to, from)) => (amount.checked_mul(to).ok_or(RoundingError::Overflow)?, from),
        None => (amount, U256::ONE),
    };
    floor_to_asset_units(numerator, denominator, COST_DECIMALS, asset_decimals)
}

/// The last second a called right may still be settled: `called_at` plus its notice period.
#[must_use]
pub fn settlement_deadline_of(called_at: u64, notice_period: u32) -> u64 {
    called_at.saturating_add(u64::from(notice_period))
}

/// Who pays a settlement, in which asset, and against which VWAP snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementPayment {
    pub payer: Address,
    pub asset: Address,
    pub snapshot_id: U256,
}

/// Which of a right's two currencies a payment asset is denominated in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentCurrency {
    Reference,
    Issuance,
}

impl PaymentCurrency {
    /// The cross rate and its snapshot a payment converts at. Only the issuance
    /// rail converts, so only it calls `cross_rate`.
    pub fn conversion<R, S, E>(
        self,
        cross_rate: impl FnOnce() -> Result<(R, S), E>,
    ) -> Result<(Option<R>, Option<S>), E> {
        match self {
            Self::Reference => Ok((None, None)),
            Self::Issuance => {
                let (rate, snapshot) = cross_rate()?;
                Ok((Some(rate), Some(snapshot)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(value: u128) -> U256 {
        U256::from(value)
    }

    #[test]
    fn same_scale_passes_through() {
        assert_eq!(
            floor_to_asset_units(u(1_234_567), U256::ONE, 6, 6),
            Ok(u(1_234_567))
        );
    }

    #[test]
    fn a_wider_asset_scales_up_exactly() {
        assert_eq!(
            floor_to_asset_units(u(7), U256::ONE, 6, 18),
            Ok(u(7_000_000_000_000))
        );
    }

    #[test]
    fn a_narrower_asset_floors_the_dust_away() {
        // 1.999999 units of a six-decimal cost into a zero-decimal asset.
        assert_eq!(
            floor_to_asset_units(u(1_999_999), U256::ONE, 6, 0),
            Ok(U256::ONE)
        );
    }

    #[test]
    fn the_rate_leg_is_floored_together_with_the_scaling() {
        // 7 / 3 scaled from 0 to 1 decimal: floor(7 * 10 / 3) = floor(70 / 3) = 23.
        // Prematurely flooring the rate leg before scaling (floor(7 / 3) * 10)
        // would have produced 20.
        assert_eq!(floor_to_asset_units(u(7), u(3), 0, 1), Ok(u(23)));
    }

    #[test]
    fn a_positive_cost_never_floors_to_nothing() {
        assert_eq!(
            floor_to_asset_units(U256::ONE, U256::ONE, COST_DECIMALS, 0),
            Err(RoundingError::RoundsToZero)
        );
        assert_eq!(
            floor_to_asset_units(U256::ZERO, U256::ONE, COST_DECIMALS, 0),
            Ok(U256::ZERO)
        );
    }

    #[test]
    fn refuses_what_it_cannot_express() {
        assert_eq!(
            floor_to_asset_units(u(1), U256::ONE, 6, 19),
            Err(RoundingError::UnsupportedDecimals(19))
        );
        assert_eq!(
            floor_to_asset_units(u(1), U256::ZERO, 6, 6),
            Err(RoundingError::ZeroRate)
        );
        assert_eq!(
            floor_to_asset_units(U256::MAX, U256::ONE, 6, 18),
            Err(RoundingError::Overflow)
        );
    }

    #[test]
    fn a_cost_is_entry_times_load_times_units() {
        let full = SettlementCost {
            entry_price_minor: u(2_000_000),
            load_minor: u(3_000_000),
            units: u(2),
        };
        // 2.0 x 3.0 x 2 = 12.0 in a six-decimal asset.
        assert_eq!(settlement_cost_units(&full, None, 6), Ok(u(12_000_000)));
        // At COEN/issuance 3 over COEN/reference 2 the issuance asset pays 1.5x.
        assert_eq!(
            settlement_cost_units(&full, Some((u(3), u(2))), 6),
            Ok(u(18_000_000))
        );
        let dust = SettlementCost {
            entry_price_minor: U256::ONE,
            load_minor: U256::ONE,
            units: U256::ONE,
        };
        assert_eq!(settlement_cost_units(&dust, None, 6), Ok(U256::ONE));
    }

    #[test]
    fn the_minimum_precedes_asset_and_currency_conversion() {
        let at = |entry: u64, load: u64| SettlementCost {
            entry_price_minor: u(entry.into()),
            load_minor: u(load.into()),
            units: U256::ONE,
        };
        let dust = at(19, 25_629);
        for (decimals, rate, expected) in [
            (6, None, 1_u64),
            (18, None, 1_000_000_000_000),
            (6, Some((2, 1)), 2),
            (18, Some((1, 2)), 500_000_000_000),
        ] {
            let rate = rate.map(|(to, from): (u128, u128)| (u(to), u(from)));
            assert_eq!(
                settlement_cost_units(&dust, rate, decimals),
                Ok(U256::from(expected))
            );
        }
        // A positive payment the asset cannot represent is refused, after a conversion too.
        for (decimals, rate) in [(0, None), (6, Some((U256::ONE, u(2))))] {
            assert_eq!(
                settlement_cost_units(&dust, rate, decimals),
                Err(RoundingError::RoundsToZero)
            );
        }
        // Costs above the minimum keep their sub-minor-unit precision.
        assert_eq!(
            settlement_cost_units(&at(1_500_001, 1), None, 18),
            Ok(u(1_500_001_000_000))
        );
        assert_eq!(settlement_cost_units(&at(0, 1), None, 6), Ok(U256::ZERO));
        assert_eq!(settlement_cost_units(&at(1, 0), None, 6), Ok(U256::ZERO));
        let huge = SettlementCost {
            entry_price_minor: U256::MAX,
            ..at(1, 2)
        };
        assert!(settlement_cost_units(&huge, None, 6).is_err());
        assert!(settlement_cost_units(&dust, Some((U256::ONE, U256::ZERO)), 6).is_err());
    }

    #[test]
    fn only_the_issuance_rail_converts() {
        let unavailable = || Err::<(U256, u8), _>("unavailable");
        assert_eq!(
            PaymentCurrency::Reference.conversion(unavailable),
            Ok((None, None))
        );
        assert_eq!(
            PaymentCurrency::Issuance.conversion(unavailable),
            Err("unavailable")
        );
        assert_eq!(
            PaymentCurrency::Issuance.conversion(|| Ok::<_, &str>((u(2), 7u8))),
            Ok((Some(u(2)), Some(7u8)))
        );
    }
}
