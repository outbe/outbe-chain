//! Offer computation - economics. Pure, deterministic, integer (U256) only.
//!
//! The settlement->nominal computation runs **only in the enclave**. The host does not
//! recompute private economics. It forwards the public pricing inputs and uses the
//! enclave result.
//!
//!   - Arithmetic is checked (no panics, per project safety rules). On overflow the
//!     enclave rejects the offer and does not abort.
//!   - The exact WorldwideDay VWAP legs and reference S-curve are public inputs. The
//!     node reads them from committed Oracle state. They are identical on every validator.
//!
//! Settlement arithmetic uses checked `U256` operations. The module-level lint
//! below rejects floating-point arithmetic.

// Deny floating-point arithmetic in the enclave economics module.
// `clippy::` tool lints are accepted (ignored) by plain rustc.
#![deny(clippy::float_arithmetic)]

use alloy_primitives::{U256, U512};
use outbe_core_common::confidential_tee::ComputeError;
use outbe_system_common::units::SCALE_1E6_U256;

type AmountResult<T> = Result<T, ComputeError>;

/// Canonical offer amount parsed once before the ZK/non-ZK paths diverge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalAmount {
    pub base: u64,
    pub micro: u64,
    pub amount_minor: U256,
}

/// Computes the S-curve pricing contract of an offer:
///
/// `effective_ref = max(reference_vwap, reference_scurve)`
/// `nominal = floor(amount * 1_000_000 * reference_vwap /
///                  (issuance_vwap * effective_ref))`
///
/// The function widens intermediate products to 512 bits. Overflow, missing required
/// VWAPs, and a positive result rounded to zero reject the offer.
pub fn compute_nominal(
    amount_minor: U256,
    issuance_wwd_vwap_minor: U256,
    reference_wwd_vwap_minor: U256,
    reference_scurve_minor: U256,
) -> AmountResult<(U256, U256)> {
    if issuance_wwd_vwap_minor.is_zero() {
        return Err(ComputeError::IssuanceVwapZero);
    }
    if reference_wwd_vwap_minor.is_zero() {
        return Err(ComputeError::ReferenceVwapZero);
    }
    let effective_reference_price_minor = reference_wwd_vwap_minor.max(reference_scurve_minor);
    let numerator = U512::from(amount_minor)
        .checked_mul(U512::from(SCALE_1E6_U256))
        .and_then(|value| value.checked_mul(U512::from(reference_wwd_vwap_minor)))
        .ok_or(ComputeError::NominalAmountOverflow)?;
    let denominator = U512::from(issuance_wwd_vwap_minor)
        .checked_mul(U512::from(effective_reference_price_minor))
        .ok_or(ComputeError::NominalDenominatorOverflow)?;
    let quotient = numerator / denominator;
    let nominal = U256::checked_from_limbs_slice(quotient.as_limbs())
        .ok_or(ComputeError::NominalAmountOverflow)?;
    if !amount_minor.is_zero() && nominal.is_zero() {
        return Err(ComputeError::NominalRoundsToZero);
    }
    Ok((nominal, effective_reference_price_minor))
}

fn parse_canonical_u64(value: &str, field: &'static str) -> AmountResult<u64> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| ComputeError::NonCanonicalU64 { field })?;
    if parsed.to_string() != value {
        return Err(ComputeError::NonCanonicalU64 { field });
    }
    Ok(parsed)
}

/// Parse the `amount_base`/`amount_micro` wire fields into the canonical
/// six-decimal offer amount. `amount_base` is a whole unsigned `u64` and the
/// `amount_micro` field is the raw remainder `0..999_999`.
pub fn parse_canonical_amount(
    base_amount: &str,
    micro_amount: &str,
) -> AmountResult<CanonicalAmount> {
    let base = parse_canonical_u64(base_amount, "amount_base")?;
    let micro = parse_canonical_u64(micro_amount, "amount_micro")?;
    if U256::from(micro) >= SCALE_1E6_U256 {
        return Err(ComputeError::AmountMicroRange);
    }
    let amount_minor = U256::from(base)
        .checked_mul(SCALE_1E6_U256)
        .and_then(|value| value.checked_add(U256::from(micro)))
        .ok_or(ComputeError::AmountOverflow)?;
    Ok(CanonicalAmount {
        base,
        micro,
        amount_minor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct CanonicalAmountCases {
        accepted_base: Vec<String>,
        rejected_base: Vec<String>,
        accepted_micro: Vec<String>,
        rejected_micro: Vec<String>,
    }

    fn canonical_amount_cases() -> CanonicalAmountCases {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../testdata/offer/canonical-amounts-v1.json"
        )))
        .unwrap()
    }

    fn parse_amount_minor(base: &str, micro: &str) -> AmountResult<U256> {
        Ok(parse_canonical_amount(base, micro)?.amount_minor)
    }

    #[test]
    fn normalize_canonical_base_and_micro_to_six_decimal_units() {
        let cases = canonical_amount_cases();
        for base in cases.accepted_base {
            assert!(
                parse_amount_minor(&base, "0").is_ok(),
                "rejected canonical base {base:?}"
            );
        }
        for micro in cases.accepted_micro {
            assert!(
                parse_amount_minor("1", &micro).is_ok(),
                "rejected canonical micro {micro:?}"
            );
        }
        assert_eq!(
            parse_amount_minor("1", "500000").unwrap(),
            U256::from(1_500_000u64)
        );
        assert_eq!(
            parse_amount_minor("100", "0").unwrap(),
            U256::from(100u64) * SCALE_1E6_U256
        );
        assert_eq!(
            parse_amount_minor(&u64::MAX.to_string(), "999999").unwrap(),
            U256::from(u64::MAX) * SCALE_1E6_U256 + U256::from(999_999u64)
        );
    }

    #[test]
    fn normalize_rejects_noncanonical_base_in_both_amount_fields() {
        let cases = canonical_amount_cases();
        for base in cases.rejected_base {
            assert!(
                parse_amount_minor(&base, "0").is_err(),
                "non-canonical amount_base {base:?} was accepted"
            );
        }
        for micro in cases.rejected_micro {
            assert!(
                parse_amount_minor("1", &micro).is_err(),
                "non-canonical amount_micro {micro:?} was accepted"
            );
        }
    }

    #[test]
    fn same_currency_without_a_higher_curve_preserves_scale_six_nominal() {
        // amount=100 COEN, price=2.0 -> 50 COEN, all expressed as raw units.
        let amount = U256::from(100u64) * SCALE_1E6_U256;
        let price = U256::from(2u64) * SCALE_1E6_U256;
        assert_eq!(
            compute_nominal(amount, price, price, U256::ZERO).unwrap(),
            (U256::from(50u64) * SCALE_1E6_U256, price)
        );
        assert_eq!(
            compute_nominal(amount, price, price, SCALE_1E6_U256).unwrap(),
            (U256::from(50u64) * SCALE_1E6_U256, price),
            "a lower reference curve must not change the effective price"
        );
    }

    #[test]
    fn cross_currency_golden_applies_only_the_reference_curve_brake() {
        assert_eq!(
            compute_nominal(
                U256::from(410_000u64),
                U256::from(10_250_000u64),
                U256::from(250_000u64),
                U256::from(320_000u64),
            )
            .unwrap(),
            (U256::from(31_250u64), U256::from(320_000u64))
        );
    }

    #[test]
    fn pricing_rejects_zero_required_inputs_overflow_and_positive_to_zero() {
        for inputs in [
            (U256::ONE, U256::ZERO, U256::ONE, U256::ZERO),
            (U256::ONE, U256::ONE, U256::ZERO, U256::ZERO),
        ] {
            assert!(compute_nominal(inputs.0, inputs.1, inputs.2, inputs.3).is_err());
        }
        assert!(compute_nominal(U256::MAX, U256::ONE, U256::MAX, U256::ZERO).is_err());
        assert!(
            compute_nominal(U256::ONE, U256::from(2_000_000u64), U256::ONE, U256::ONE,).is_err()
        );
    }
}
