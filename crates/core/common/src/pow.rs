//! Shared proof-of-work gate for entity mining (Gem, Nod, ...).
//!
//! All factories use the same SHA256 PoW scheme so off-chain miners can reuse
//! a single tooling implementation: the digest is taken over
//! `ascii(hex(id_be32)) || nonce.to_be_bytes::<8>()` and the hash must have
//! [`POW_DIFFICULTY`] leading zero bytes.

use alloy_primitives::U256;
use ring::digest::{digest, SHA256};

/// PoW difficulty: number of leading zero bytes required in the SHA256 hash.
/// Identical across all entity factories.
pub const POW_DIFFICULTY: usize = 1;

/// Proof-of-work failure modes. Factories map these onto their own error enums;
/// kept exhaustive so a new variant forces every mapping site to handle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowError {
    /// `nonce` does not fit in the 8-byte big-endian encoding used by the hash.
    NonceExceedsUint64Range,
    /// The computed hash does not have [`POW_DIFFICULTY`] leading zero bytes.
    InsufficientProofOfWork,
}

/// SHA256 over `ascii(hex(id_be32)) || nonce.to_be_bytes::<8>()`.
///
/// The id is formatted as a 64-char lowercase hex string of its 32-byte
/// big-endian representation, matching `format_gem_id` / `format_nod_id`.
pub fn compute_pow_hash(id: U256, nonce: U256) -> Result<[u8; 32], PowError> {
    if nonce > U256::from(u64::MAX) {
        return Err(PowError::NonceExceedsUint64Range);
    }
    let nonce_bytes = nonce.to::<u64>().to_be_bytes();
    let id_str = hex::encode(id.to_be_bytes::<32>());
    let mut data = Vec::with_capacity(id_str.len() + nonce_bytes.len());
    data.extend_from_slice(id_str.as_bytes());
    data.extend_from_slice(&nonce_bytes);
    let digest = digest(&SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    Ok(out)
}

/// Validates that [`compute_pow_hash`] has [`POW_DIFFICULTY`] leading zero
/// bytes.
pub fn validate_pow(id: U256, nonce: U256) -> Result<(), PowError> {
    let hash = compute_pow_hash(id, nonce)?;
    for byte in &hash[..POW_DIFFICULTY] {
        if *byte != 0 {
            return Err(PowError::InsufficientProofOfWork);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force the lowest nonce that satisfies `validate_pow(id, _)` for
    /// the current `POW_DIFFICULTY`. With difficulty=1 the expected loop length
    /// is ~256 iterations.
    fn find_valid_nonce(id: U256) -> U256 {
        for n in 0u64..100_000 {
            let nonce = U256::from(n);
            if validate_pow(id, nonce).is_ok() {
                return nonce;
            }
        }
        panic!("no valid nonce found in 100k attempts")
    }

    #[test]
    fn compute_pow_hash_matches_sha256_string_id_plus_u64_nonce() {
        let id = U256::from(0x1234_5678u64);
        let nonce = U256::from(42u64);
        let got = compute_pow_hash(id, nonce).unwrap();

        let mut data = hex::encode(id.to_be_bytes::<32>()).into_bytes();
        data.extend_from_slice(&42u64.to_be_bytes());
        let expected = digest(&SHA256, &data);

        assert_eq!(got.as_ref(), expected.as_ref());
    }

    #[test]
    fn nonce_above_u64_max_is_rejected() {
        let id = U256::from(1u64);
        let nonce = U256::from(u64::MAX) + U256::from(1u64);
        assert_eq!(
            compute_pow_hash(id, nonce),
            Err(PowError::NonceExceedsUint64Range)
        );
        assert_eq!(
            validate_pow(id, nonce),
            Err(PowError::NonceExceedsUint64Range)
        );
    }

    #[test]
    fn valid_nonce_passes_and_neighbours_likely_fail() {
        let id = U256::from(0xABCDu64);
        let nonce = find_valid_nonce(id);
        assert!(validate_pow(id, nonce).is_ok());
    }

    #[test]
    fn insufficient_pow_is_rejected() {
        let id = U256::from(7u64);
        // Find a nonce whose first byte is non-zero (fails difficulty=1).
        for n in 0u64..100_000 {
            let nonce = U256::from(n);
            if compute_pow_hash(id, nonce).unwrap()[0] != 0 {
                assert_eq!(
                    validate_pow(id, nonce),
                    Err(PowError::InsufficientProofOfWork)
                );
                return;
            }
        }
        panic!("no failing nonce found");
    }
}
