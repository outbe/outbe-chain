//! Shared proof-of-work gate for entity mining (Gem, Nod, ...).
//!
//! All factories use the same SHA256 PoW scheme so off-chain miners can reuse
//! a single tooling implementation: the digest is taken over
//! `ascii(hex(id_be32)) || nonce.to_be_bytes()` and the hash must have
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
    /// The computed hash does not have [`POW_DIFFICULTY`] leading zero bytes.
    InsufficientProofOfWork,
}

/// SHA256 over `ascii(hex(id_be32)) || nonce.to_be_bytes()`.
///
/// The id is formatted as a 64-char lowercase hex string of its 32-byte
/// big-endian representation, matching `format_gem_id` / `format_nod_id`.
pub fn compute_pow_hash(id: U256, nonce: u64) -> [u8; 32] {
    compute_pow_hash_bytes(&id.to_be_bytes::<32>(), nonce)
}

fn compute_pow_hash_bytes(id: &[u8], nonce: u64) -> [u8; 32] {
    let nonce_bytes = nonce.to_be_bytes();
    let id_str = hex::encode(id);
    let mut data = Vec::with_capacity(id_str.len() + nonce_bytes.len());
    data.extend_from_slice(id_str.as_bytes());
    data.extend_from_slice(&nonce_bytes);
    let digest = digest(&SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Validates that [`compute_pow_hash`] has [`POW_DIFFICULTY`] leading zero
/// bytes.
pub fn validate_pow(id: U256, nonce: u64) -> Result<(), PowError> {
    let hash = compute_pow_hash(id, nonce);
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
    fn find_valid_nonce(id: U256) -> u64 {
        for nonce in 0u64..100_000 {
            if validate_pow(id, nonce).is_ok() {
                return nonce;
            }
        }
        panic!("no valid nonce found in 100k attempts")
    }

    #[test]
    fn compute_pow_hash_matches_sha256_string_id_plus_u64_nonce() {
        let id = U256::from(0x1234_5678u64);
        let got = compute_pow_hash(id, 42);

        let mut data = hex::encode(id.to_be_bytes::<32>()).into_bytes();
        data.extend_from_slice(&42u64.to_be_bytes());
        let expected = digest(&SHA256, &data);

        assert_eq!(got.as_ref(), expected.as_ref());
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
        for nonce in 0u64..100_000 {
            if compute_pow_hash(id, nonce)[0] != 0 {
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
