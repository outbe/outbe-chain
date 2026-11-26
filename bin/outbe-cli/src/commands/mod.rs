pub mod chain;
pub mod monitor;
pub mod rad;
pub mod slash;
pub mod stake_identity;
pub mod staking;
pub mod tee;
pub mod validator;
pub mod vote;

use crate::errors::CommandInputError;
use crate::tx::TxSigner;
use alloy_primitives::U256;
use eyre::Result;

/// Parse a non-zero amount in decimal base units.
pub fn parse_amount(amount: &str) -> Result<U256> {
    let amount = U256::from_str_radix(amount, 10).map_err(CommandInputError::InvalidAmount)?;
    if amount.is_zero() {
        return Err(CommandInputError::ZeroAmount.into());
    }
    Ok(amount)
}

/// Parse a decimal base-unit amount that may be zero (used only by `staking unstake`).
pub fn parse_amount_allow_zero(amount: &str) -> Result<U256> {
    U256::from_str_radix(amount, 10).map_err(|e| CommandInputError::InvalidAmount(e).into())
}

pub fn require_signer(private_key: Option<&str>) -> Result<TxSigner> {
    let key = private_key.ok_or(CommandInputError::PrivateKeyRequired)?;
    TxSigner::new(key)
}

fn format_decimals(value: alloy_primitives::U256, decimals: u8) -> String {
    use alloy_primitives::U256;

    if value.is_zero() {
        return "0".to_string();
    }

    let divisor = U256::from(10u64).pow(U256::from(decimals));
    let whole = value / divisor;
    let frac = value % divisor;

    if frac.is_zero() {
        format!("{whole}")
    } else {
        let frac_str = format!("{frac:0>width$}", width = usize::from(decimals));
        let trimmed = frac_str.trim_end_matches('0');
        format!("{whole}.{trimmed}")
    }
}

/// Format a raw native COEN amount in 18-decimal base units.
pub fn format_coen_amount(value: alloy_primitives::U256) -> String {
    format_decimals(value, 18)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    #[test]
    fn test_format_unit_zero() {
        assert_eq!(format_coen_amount(U256::ZERO), "0");
    }

    #[test]
    fn test_format_unit_one_coen() {
        assert_eq!(
            format_coen_amount(U256::from(1_000_000_000_000_000_000u128)),
            "1"
        );
    }

    #[test]
    fn test_format_unit_large_whole() {
        let val = U256::from(1_000_000_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1000");
    }

    #[test]
    fn test_format_unit_fractional() {
        let val = U256::from(1_500_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1.5");
    }

    #[test]
    fn test_format_unit_pure_fraction() {
        let val = U256::from(500_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "0.5");
    }

    #[test]
    fn test_format_unit_one_native_unit() {
        assert_eq!(format_coen_amount(U256::from(1u64)), "0.000000000000000001");
    }

    #[test]
    fn test_format_unit_trailing_zeros_trimmed() {
        let val = U256::from(1_200_000_000_000_000_000u128);
        assert_eq!(format_coen_amount(val), "1.2");
    }

    #[test]
    fn test_format_unit_all_decimal_places() {
        let val = U256::from(999_999_999_999_999_999u128);
        assert_eq!(format_coen_amount(val), "0.999999999999999999");
    }

    #[test]
    fn test_format_unit_max_u256_never_panics() {
        let result = format_coen_amount(U256::MAX);
        assert!(!result.is_empty());
        assert!(result.contains('.'));
    }
}
