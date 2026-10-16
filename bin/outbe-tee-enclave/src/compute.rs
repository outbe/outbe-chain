//! Tribute computation - economics + `tribute_id`. Pure, deterministic, integer
//! (U256) only.
//!
//! The settlement->nominal computation runs **only in the enclave**. The host
//! `outbe-tributefactory` runtime does not recompute private economics. It forwards
//! the public pricing inputs and uses the enclave result.
//!
//!   - Arithmetic is checked (no panics, per project safety rules). On overflow the
//!     enclave rejects the offer and does not abort.
//!   - The exact WorldwideDay VWAP legs and reference S-curve are public inputs. The
//!     node reads them from committed Oracle state. They are identical on every validator.
//!
//! The token id is a Poseidon2-BN254 digest of the owner and the WorldwideDay. The host
//! recomputes the same digest and derives the tribute id from it.
//!
//! Settlement arithmetic uses checked `U256` operations. The module-level lint
//! below rejects floating-point arithmetic.

// Deny floating-point arithmetic in the enclave economics module.
// `clippy::` tool lints are accepted (ignored) by plain rustc.
#![deny(clippy::float_arithmetic)]

use alloy_primitives::{Address, B256, U256, U512};
use outbe_primitives::units::SCALE_1E6_U256;
use outbe_tee::protocol::WorldwideDay;

use crate::errors::{ComputeError, EnclaveResult};

/// Canonical Tribute amount parsed once before the ZK/non-ZK paths diverge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalAmount {
    pub(crate) base: u64,
    pub(crate) micro: u64,
    pub(crate) amount_minor: U256,
}

/// Parse the `tribute_draft_id` (a 32-byte value as a hex string, optional `0x`
/// prefix) into raw bytes. Mirrors host `parse_su_hashes`.
fn parse_draft_id(draft_id: &str) -> EnclaveResult<[u8; 32]> {
    let hex_str = draft_id.strip_prefix("0x").unwrap_or(draft_id);
    let bytes = hex::decode(hex_str).map_err(|_| ComputeError::InvalidDraftIdHex {
        draft_id: draft_id.to_owned(),
    })?;
    if bytes.len() != 32 {
        return Err(ComputeError::DraftIdLength { len: bytes.len() }.into());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Returns the token id `Poseidon2(owner, worldwide_day)`, BN254.
/// The digest depends on `(owner, worldwide_day)` ALONE. This property enforces
/// the one-tribute-per-owner-per-day invariant. The host recomputes this digest and
/// derives the tribute id from the day and the digest tail
/// (`WwdEntityId::from_day_and_digest`). A second offer for the same owner and day
/// gives the same tribute id, so the host's `get_tribute` check rejects it
/// (`TributeAlreadyExists`). This function still checks the `tribute_draft_id`
/// (must be 32-byte hex) but intentionally does NOT mix it into the id. Mixing it in
/// made the id per-offer-unique and silently allowed duplicate tributes per owner per day.
///
/// Both identity inputs fit below the BN254 scalar modulus:
/// - `owner`: address left-padded to 32 bytes.
/// - `worldwide_day`: its `YYYYMMDD` word as 32-byte big-endian.
pub fn compute_token_id(
    owner: Address,
    worldwide_day: WorldwideDay,
    draft_id: &str,
) -> EnclaveResult<B256> {
    // Validate the draft id (reject malformed input) but keep it out of the hash.
    parse_draft_id(draft_id)?;
    outbe_primitives::wwd_entity_id::derive_poseidon_digest(owner, worldwide_day)
        .map_err(|error| ComputeError::from(error).into())
}

/// Computes the Tribute-only S-curve pricing contract:
///
/// `effective_ref = max(reference_vwap, reference_scurve)`
/// `nominal = floor(amount * 1_000_000 * reference_vwap /
///                  (issuance_vwap * effective_ref))`
///
/// The function widens intermediate products to 512 bits. Overflow, missing required
/// VWAPs, and a positive result rounded to zero reject the offer.
pub(crate) fn compute_nominal(
    amount_minor: U256,
    issuance_wwd_vwap_minor: U256,
    reference_wwd_vwap_minor: U256,
    reference_scurve_minor: U256,
) -> EnclaveResult<(U256, U256)> {
    if issuance_wwd_vwap_minor.is_zero() {
        return Err(ComputeError::IssuanceVwapZero.into());
    }
    if reference_wwd_vwap_minor.is_zero() {
        return Err(ComputeError::ReferenceVwapZero.into());
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
        return Err(ComputeError::NominalRoundsToZero.into());
    }
    Ok((nominal, effective_reference_price_minor))
}

fn parse_canonical_u64(value: &str, field: &'static str) -> EnclaveResult<u64> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| ComputeError::NonCanonicalU64 { field })?;
    if parsed.to_string() != value {
        return Err(ComputeError::NonCanonicalU64 { field }.into());
    }
    Ok(parsed)
}

/// Parse the `amount_base`/`amount_micro` wire fields into the canonical
/// six-decimal Tribute amount. `amount_base` is a whole unsigned `u64` and the
/// `amount_micro` field is the raw remainder `0..999_999`.
pub(crate) fn parse_canonical_amount(
    base_amount: &str,
    micro_amount: &str,
) -> EnclaveResult<CanonicalAmount> {
    let base = parse_canonical_u64(base_amount, "amount_base")?;
    let micro = parse_canonical_u64(micro_amount, "amount_micro")?;
    if U256::from(micro) >= SCALE_1E6_U256 {
        return Err(ComputeError::AmountMicroRange.into());
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
            "/../../testdata/tribute/canonical-amounts-v1.json"
        )))
        .unwrap()
    }

    fn parse_amount_minor(base: &str, micro: &str) -> EnclaveResult<U256> {
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

    const DRAFT_A: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    const DRAFT_B: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";
    const DAY: WorldwideDay = WorldwideDay::new(20250115);
    const NEXT_DAY: WorldwideDay = WorldwideDay::new(20250116);

    #[test]
    fn token_id_deterministic_and_input_bound() {
        let a = Address::repeat_byte(0x11);
        let b = Address::repeat_byte(0x22);
        let base = compute_token_id(a, DAY, DRAFT_A).unwrap();
        assert_eq!(base, compute_token_id(a, DAY, DRAFT_A).unwrap());
        assert_ne!(base, compute_token_id(b, DAY, DRAFT_A).unwrap()); // owner
        assert_ne!(base, compute_token_id(a, NEXT_DAY, DRAFT_A).unwrap()); // day

        // draft_id is deliberately NOT bound into the id. The same owner+day yields
        // the same id regardless of draft, which enforces one-per-owner-per-day.
        assert_eq!(base, compute_token_id(a, DAY, DRAFT_B).unwrap());
    }

    #[test]
    fn token_id_rejects_bad_draft_id() {
        assert!(compute_token_id(Address::ZERO, DAY, "not-hex").is_err());
        assert!(compute_token_id(Address::ZERO, DAY, "0x1234").is_err()); // not 32 bytes
    }

    #[test]
    fn token_id_matches_chain_digest() {
        let owner = Address::repeat_byte(0x42);
        assert_eq!(
            compute_token_id(owner, DAY, DRAFT_A).unwrap(),
            outbe_primitives::wwd_entity_id::derive_poseidon_digest(owner, DAY).unwrap()
        );
    }
}
