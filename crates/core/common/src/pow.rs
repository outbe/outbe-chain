//! Shared proof-of-work gate for entity mining.
//!
//! [`POW_DIFFICULTY`] and [`meets_difficulty`] are the protocol's verdict on a
//! work hash, whatever preimage produced it.
//!
//! Nod hashes
//! `right_id_be32 || owner_20 || mining_sequence_be8 || nonce_be8` with
//! [`SINGLE_EXERCISE_SEQUENCE`]. Gem still hashes
//! `id_be32 || nonce_be8` until it moves onto the same preimage. Intex builds
//! its own preimage (owner, amount, series, sequence, nonce) and only reuses
//! the difficulty check.

use alloy_primitives::{Address, U256};
use ring::digest::{digest, SHA256};

/// PoW difficulty: number of leading zero bytes required in the SHA256 hash.
/// Identical across all entity factories.
pub const POW_DIFFICULTY: usize = 1;

/// Mining sequence for a right that is exercised once (Nod today; Gem when it
/// adopts the shared preimage).
pub const SINGLE_EXERCISE_SEQUENCE: u64 = 0;

/// Proof-of-work failure modes. Factories map these onto their own error enums;
/// kept exhaustive so a new variant forces every mapping site to handle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowError {
    /// The computed hash does not have [`POW_DIFFICULTY`] leading zero bytes.
    InsufficientProofOfWork,
}

/// SHA256 over `right_id_be32 || owner_20 || mining_sequence_be8 || nonce_be8`.
pub fn compute_mining_pow_hash(
    right_id: U256,
    owner: Address,
    mining_sequence: u64,
    nonce: u64,
) -> [u8; 32] {
    let mut data = [0u8; 68];
    data[..32].copy_from_slice(&right_id.to_be_bytes::<32>());
    data[32..52].copy_from_slice(owner.as_slice());
    data[52..60].copy_from_slice(&mining_sequence.to_be_bytes());
    data[60..68].copy_from_slice(&nonce.to_be_bytes());
    let digest = digest(&SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Validates that [`compute_mining_pow_hash`] has [`POW_DIFFICULTY`] leading
/// zero bytes.
pub fn validate_mining_pow(
    right_id: U256,
    owner: Address,
    mining_sequence: u64,
    nonce: u64,
) -> Result<(), PowError> {
    meets_difficulty(&compute_mining_pow_hash(
        right_id,
        owner,
        mining_sequence,
        nonce,
    ))
}

/// SHA256 over the raw `id.to_be_bytes::<32>() || nonce.to_be_bytes()`.
///
/// Gem still uses this preimage. New mining rights should call
/// [`compute_mining_pow_hash`].
pub fn compute_pow_hash(id: U256, nonce: u64) -> [u8; 32] {
    let mut data = [0u8; 40];
    data[..32].copy_from_slice(&id.to_be_bytes::<32>());
    data[32..].copy_from_slice(&nonce.to_be_bytes());
    let digest = digest(&SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// The protocol's single verdict on a work hash, whatever preimage produced it:
/// a right whose own scheme differs still weighs its work here.
pub fn meets_difficulty(hash: &[u8; 32]) -> Result<(), PowError> {
    if hash[..POW_DIFFICULTY].iter().any(|byte| *byte != 0) {
        return Err(PowError::InsufficientProofOfWork);
    }
    Ok(())
}

/// Validates that [`compute_pow_hash`] has [`POW_DIFFICULTY`] leading zero
/// bytes.
pub fn validate_pow(id: U256, nonce: u64) -> Result<(), PowError> {
    meets_difficulty(&compute_pow_hash(id, nonce))
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

    fn find_valid_mining_nonce(right_id: U256, owner: Address) -> u64 {
        for nonce in 0u64..100_000 {
            if validate_mining_pow(right_id, owner, SINGLE_EXERCISE_SEQUENCE, nonce).is_ok() {
                return nonce;
            }
        }
        panic!("no valid mining nonce found in 100k attempts")
    }

    #[test]
    fn compute_pow_hash_matches_sha256_of_raw_id_bytes_plus_u64_nonce() {
        let id = U256::from(0x1234_5678u64);
        let got = compute_pow_hash(id, 42);

        let mut data = id.to_be_bytes::<32>().to_vec();
        data.extend_from_slice(&42u64.to_be_bytes());
        let expected = digest(&SHA256, &data);

        assert_eq!(got.as_ref(), expected.as_ref());
    }

    #[test]
    fn mining_pow_hash_is_right_id_owner_sequence_and_nonce() {
        let right_id = U256::from(0x1234_5678u64);
        let owner = Address::repeat_byte(0x11);
        let got = compute_mining_pow_hash(right_id, owner, SINGLE_EXERCISE_SEQUENCE, 42);

        let mut data = right_id.to_be_bytes::<32>().to_vec();
        data.extend_from_slice(owner.as_slice());
        data.extend_from_slice(&SINGLE_EXERCISE_SEQUENCE.to_be_bytes());
        data.extend_from_slice(&42u64.to_be_bytes());
        let expected = digest(&SHA256, &data);

        assert_eq!(got.as_ref(), expected.as_ref());
        assert_ne!(got, compute_pow_hash(right_id, 42));
        assert_ne!(
            got,
            compute_mining_pow_hash(
                right_id,
                Address::repeat_byte(0x22),
                SINGLE_EXERCISE_SEQUENCE,
                42
            )
        );
        assert_ne!(got, compute_mining_pow_hash(right_id, owner, 1, 42));
    }

    #[test]
    fn valid_nonce_passes_and_neighbours_likely_fail() {
        let id = U256::from(0xABCDu64);
        let nonce = find_valid_nonce(id);
        assert!(validate_pow(id, nonce).is_ok());
    }

    #[test]
    fn mining_pow_accepts_a_solved_nonce_for_that_owner() {
        let right_id = U256::from(0xABCDu64);
        let owner = Address::repeat_byte(0x33);
        let other = Address::repeat_byte(0x44);
        let nonce = find_valid_mining_nonce(right_id, owner);
        assert!(validate_mining_pow(right_id, owner, SINGLE_EXERCISE_SEQUENCE, nonce).is_ok());
        assert_ne!(
            compute_mining_pow_hash(right_id, owner, SINGLE_EXERCISE_SEQUENCE, nonce),
            compute_mining_pow_hash(right_id, other, SINGLE_EXERCISE_SEQUENCE, nonce)
        );
    }

    #[test]
    fn the_verdict_reads_only_the_leading_bytes() {
        let mut hash = [0u8; 32];
        hash[31] = 0xFF;
        assert!(meets_difficulty(&hash).is_ok());
        hash[0] = 1;
        assert_eq!(
            meets_difficulty(&hash),
            Err(PowError::InsufficientProofOfWork)
        );
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
