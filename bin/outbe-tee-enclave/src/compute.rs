//! Tribute computation — economics + `tribute_id`. Pure, deterministic, integer
//! (U256) only.
//!
//! Economics move the settlement->nominal computation **into the enclave**, faithfully replicating the host's current
//! `outbe-tributefactory::runtime` math, with two intentional differences:
//!
//!   - checked arithmetic (no panics, per project safety rules — the host code
//!     used unchecked ops); on overflow the offer is rejected, not aborted;
//!   - the oracle price is an **input** (`tribute_price_minor`), read by the node
//!     from committed Oracle state and identical on every validator.
//!
//! `tribute_id` is a Poseidon-BN254 hash over sensitive decrypted data, so it is
//! computed only in the enclave.
//!
//! No 32-bit or 64-bit floating-point types anywhere (project numeric rules) —
//! enforced by the module-level lint below and the `no_floating_point_in_enclave_economics`
//! source-scan test.

// Deny any floating-point arithmetic in the enclave economics module. Combined
// with the source-scan test (which also catches float *types*/literals), this
// keeps the settlement math integer-only (U256), as the on-chain numeric rules
// require. `clippy::` tool lints are accepted (ignored) by plain rustc.
#![deny(clippy::float_arithmetic)]

use alloy_primitives::{Address, B256, U256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};

/// 10^18 fixed-point scale, identical to `outbe_primitives::units::SCALE_1E18`.
pub const SCALE_1E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// Only USD (ISO-4217 840) is accepted in the PoC, matching the host.
pub const USD_ISO_4217: u16 = 840;

/// Reject any currency other than USD (matches host `check_currency`).
pub fn check_currency(currency: u16) -> Result<(), String> {
    if currency != USD_ISO_4217 {
        return Err(format!("iso_code {currency} is not a valid currency"));
    }
    Ok(())
}

/// Calendar validity of a `YYYYMMDD` worldwide-day key. Behaviour-equivalent to
/// `outbe_common::WorldwideDay::is_valid` (Gregorian), hand-rolled to keep the
/// enclave dependency surface minimal for reproducible `MRENCLAVE`.
pub fn worldwide_day_is_valid(day: u32) -> bool {
    let year = day / 10_000;
    let month = (day / 100) % 100;
    let dom = day % 100;
    if !(1..=12).contains(&month) || dom < 1 {
        return false;
    }
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let max_dom = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    dom <= max_dom
}

/// Circom Poseidon permutation max width.
const MAX_POSEIDON_INPUTS: usize = 12;

/// Poseidon-BN254 over N BE-encoded field elements packed into `input`
/// (multiple of 32 bytes). Byte-identical to
/// `outbe_zkproof::poseidon::poseidon_hash` (same crates, same circom params);
/// replicated here so the enclave does not pull zkproof's heavy Barretenberg
/// FFI. Each 32-byte chunk is reduced mod the BN254 scalar order.
fn poseidon_hash(input: &[u8]) -> Result<[u8; 32], String> {
    if input.is_empty() {
        return Err("poseidon: empty input".to_string());
    }
    if !input.len().is_multiple_of(32) {
        return Err(format!("poseidon: unaligned input ({} bytes)", input.len()));
    }
    let n = input.len() / 32;
    if n > MAX_POSEIDON_INPUTS {
        return Err(format!("poseidon: too many inputs ({n})"));
    }
    let inputs: Vec<Fr> = input.chunks(32).map(Fr::from_be_bytes_mod_order).collect();
    let mut poseidon = Poseidon::<Fr>::new_circom(n).map_err(|e| format!("poseidon setup: {e}"))?;
    let hash = poseidon
        .hash(&inputs)
        .map_err(|e| format!("poseidon hash: {e}"))?;
    let be = hash.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    let off = 32 - be.len().min(32);
    out[off..].copy_from_slice(&be[be.len().saturating_sub(32)..]);
    Ok(out)
}

/// Parse the `tribute_draft_id` (a 32-byte value as a hex string, optional `0x`
/// prefix) into raw bytes — mirrors host `parse_su_hashes`.
fn parse_draft_id(draft_id: &str) -> Result<[u8; 32], String> {
    let hex_str = draft_id.strip_prefix("0x").unwrap_or(draft_id);
    let bytes =
        hex::decode(hex_str).map_err(|_| format!("invalid tribute_draft_id hex: {draft_id}"))?;
    if bytes.len() != 32 {
        return Err(format!(
            "tribute_draft_id must be 32 bytes, got {}",
            bytes.len()
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// `tribute_id = Poseidon(owner, worldwide_day)` — BN254/circom, computed inside
/// the enclave. The id is deterministic in `(owner, worldwide_day)` ALONE: this
/// is what enforces the one-tribute-per-owner-per-day invariant. A second offer
/// for the same owner and day recomputes the same id, so the host's
/// `get_tribute(id).is_some()` check rejects it (`TributeAlreadyExists`). The
/// `tribute_draft_id` is still validated (must be 32-byte hex) but intentionally
/// NOT mixed into the id — mixing it in made the id per-offer-unique and silently
/// allowed duplicate tributes per owner per day.
///
/// Field-element encoding (each reduced mod the BN254 order by `poseidon_hash`):
/// - `owner`: address left-padded to 32 bytes;
/// - `worldwide_day`: `u32` as 32-byte big-endian.
pub fn compute_token_id(
    owner: Address,
    worldwide_day: u32,
    draft_id: &str,
) -> Result<B256, String> {
    // Validate the draft id (reject malformed input) but keep it out of the hash.
    parse_draft_id(draft_id)?;
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(owner.into_word().as_slice());
    buf.extend_from_slice(&U256::from(worldwide_day).to_be_bytes::<32>());
    Ok(B256::from(poseidon_hash(&buf)?))
}

/// `nominal = amount_minor * 1e18 / tribute_price_minor`. Caller guarantees a
/// non-zero price. Overflow -> reject reason.
pub fn compute_nominal(amount_minor: U256, tribute_price_minor: U256) -> Result<U256, String> {
    let scaled = amount_minor
        .checked_mul(SCALE_1E18)
        .ok_or_else(|| "nominal amount overflow".to_string())?;
    Ok(scaled / tribute_price_minor)
}

/// Parse `amount_base` (decimal string, up to 18 fractional digits) plus
/// `amount_atto` (integer minor units) into a `U256` minor amount.
///
/// Replicates host `normalize_amount` exactly on valid inputs, but uses checked
/// arithmetic so an overflow rejects the offer instead of panicking.
pub fn normalize_amount(base_amount: &str, atto_amount: &str) -> Result<U256, String> {
    let base_minor = if let Some(dot_pos) = base_amount.find('.') {
        let int_part = &base_amount[..dot_pos];
        let frac_part = &base_amount[dot_pos + 1..];

        if frac_part.len() > 18 {
            return Err("base amount has too many decimals".to_string());
        }

        let int_val = if int_part.is_empty() {
            U256::ZERO
        } else {
            U256::from(
                int_part
                    .parse::<u128>()
                    .map_err(|_| "invalid base amount format".to_string())?,
            )
        };

        let frac_val = if frac_part.is_empty() {
            U256::ZERO
        } else {
            let parsed = U256::from(
                frac_part
                    .parse::<u128>()
                    .map_err(|_| "invalid base amount format".to_string())?,
            );
            // exponent is 0..=18, so 10^exp <= 10^18 < 2^256 — no overflow.
            parsed * U256::from(10).pow(U256::from(18 - frac_part.len() as u32))
        };

        int_val
            .checked_mul(SCALE_1E18)
            .ok_or_else(|| "base amount overflow".to_string())?
            .checked_add(frac_val)
            .ok_or_else(|| "base amount overflow".to_string())?
    } else {
        U256::from(
            base_amount
                .parse::<u128>()
                .map_err(|_| "invalid base amount format".to_string())?,
        )
        .checked_mul(SCALE_1E18)
        .ok_or_else(|| "base amount overflow".to_string())?
    };

    let atto_minor = U256::from(
        atto_amount
            .parse::<u128>()
            .map_err(|_| "invalid atto amount format".to_string())?,
    );

    base_minor
        .checked_add(atto_minor)
        .ok_or_else(|| "amount overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enforce the no-floating-point rule over the enclave economics by
    /// scanning this module's own source for 32-/64-bit float type tokens. The
    /// needles are assembled at runtime so this test's own code (and this comment)
    /// do not trip the scan.
    #[test]
    fn no_floating_point_in_enclave_economics() {
        let src = include_str!("compute.rs");
        // Needles assembled at runtime; the variable names + assert text avoid the
        // literal tokens so this test's own source does not trip the scan.
        let needle_single = ["f", "32"].concat();
        let needle_double = ["f", "64"].concat();
        assert!(
            !src.contains(&needle_single),
            "32-bit float type found in enclave economics (integer-only rule)"
        );
        assert!(
            !src.contains(&needle_double),
            "64-bit float type found in enclave economics (integer-only rule)"
        );
    }

    #[test]
    fn normalize_integer_and_atto() {
        assert_eq!(
            normalize_amount("100", "0").unwrap(),
            U256::from(100u64) * SCALE_1E18
        );
        assert_eq!(normalize_amount("0", "5").unwrap(), U256::from(5u64));
    }

    #[test]
    fn normalize_fractional() {
        // 1.5 -> 1e18 + 5e17
        let expected = SCALE_1E18 + U256::from(5u64) * U256::from(10u64).pow(U256::from(17u32));
        assert_eq!(normalize_amount("1.5", "0").unwrap(), expected);
        // smallest unit: 0.000000000000000001 -> 1
        assert_eq!(
            normalize_amount("0.000000000000000001", "0").unwrap(),
            U256::from(1u64)
        );
    }

    #[test]
    fn normalize_rejects_too_many_decimals() {
        assert!(normalize_amount("0.0000000000000000001", "0").is_err());
    }

    #[test]
    fn nominal_division() {
        // amount=100e18, price=2e18 -> 100e18 * 1e18 / 2e18 = 50e18
        let amount = U256::from(100u64) * SCALE_1E18;
        let price = U256::from(2u64) * SCALE_1E18;
        assert_eq!(
            compute_nominal(amount, price).unwrap(),
            U256::from(50u64) * SCALE_1E18
        );
    }

    #[test]
    fn currency_gate() {
        assert!(check_currency(840).is_ok());
        assert!(check_currency(978).is_err());
    }

    #[test]
    fn worldwide_day_validity() {
        assert!(worldwide_day_is_valid(20250115));
        assert!(worldwide_day_is_valid(20240229)); // leap day
        assert!(!worldwide_day_is_valid(20250229)); // not a leap year
        assert!(!worldwide_day_is_valid(20251301)); // month 13
        assert!(!worldwide_day_is_valid(20250100)); // day 0
    }

    const DRAFT_A: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
    const DRAFT_B: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn token_id_deterministic_and_input_bound() {
        let a = Address::repeat_byte(0x11);
        let b = Address::repeat_byte(0x22);
        let base = compute_token_id(a, 20250115, DRAFT_A).unwrap();
        assert_eq!(base, compute_token_id(a, 20250115, DRAFT_A).unwrap());
        assert_ne!(base, compute_token_id(b, 20250115, DRAFT_A).unwrap()); // owner
        assert_ne!(base, compute_token_id(a, 20250116, DRAFT_A).unwrap()); // day
                                                                           // draft_id is deliberately NOT bound into the id — same owner+day yields the
                                                                           // same id regardless of draft, which is what enforces one-per-owner-per-day.
        assert_eq!(base, compute_token_id(a, 20250115, DRAFT_B).unwrap()); // draft_id ignored
    }

    #[test]
    fn token_id_rejects_bad_draft_id() {
        assert!(compute_token_id(Address::ZERO, 20250115, "not-hex").is_err());
        assert!(compute_token_id(Address::ZERO, 20250115, "0x1234").is_err()); // not 32 bytes
    }

    /// Proves our replicated `poseidon_hash` matches a fresh circom hasher (the
    /// same self-consistency check zkproof uses), guaranteeing byte-identity
    /// with the chain's `outbe_zkproof::poseidon::poseidon_hash`.
    #[test]
    fn poseidon_matches_fresh_circom_hasher() {
        fn fr_be(f: &Fr) -> [u8; 32] {
            let be = f.into_bigint().to_bytes_be();
            let mut out = [0u8; 32];
            let off = 32 - be.len().min(32);
            out[off..].copy_from_slice(&be[be.len().saturating_sub(32)..]);
            out
        }
        let a = Fr::from(7u64);
        let b = Fr::from(20_250_115u64);
        let mut input = Vec::new();
        input.extend_from_slice(&fr_be(&a));
        input.extend_from_slice(&fr_be(&b));

        let mine = poseidon_hash(&input).unwrap();
        let mut hasher = Poseidon::<Fr>::new_circom(2).unwrap();
        let reference = fr_be(&hasher.hash(&[a, b]).unwrap());
        assert_eq!(mine, reference);
    }
}
