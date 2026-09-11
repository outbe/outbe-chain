//! Shared proof-of-work gate for entity mining (Gem, Nod, ...).
//!
//! All factories use the same SHA256 PoW scheme so off-chain miners can reuse
//! a single tooling implementation: the digest is taken over
//! `id_be32 || owner || seq_be4 || nonce_be8` and the hash must have
//! [`POW_DIFFICULTY`] leading zero bytes. The owner is in the preimage so a
//! solution found for one right cannot serve anyone else's; the sequence is
//! zero here, for rights that are exercised once.

use alloy_primitives::{Address, U256};
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

/// SHA256 over `id_be32 || owner || seq_be4 || nonce_be8`.
pub fn compute_pow_hash(id: U256, owner: Address, nonce: u64) -> [u8; 32] {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(&id.to_be_bytes::<32>());
    data[32..52].copy_from_slice(owner.as_slice());
    data[56..].copy_from_slice(&nonce.to_be_bytes());
    let digest = digest(&SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Validates that [`compute_pow_hash`] has [`POW_DIFFICULTY`] leading zero
/// bytes.
pub fn validate_pow(id: U256, owner: Address, nonce: u64) -> Result<(), PowError> {
    let hash = compute_pow_hash(id, owner, nonce);
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
    use alloy_primitives::address;

    const OWNER: Address = address!("00000000000000000000000000000000000000a1");
    const OTHER: Address = address!("00000000000000000000000000000000000000b2");

    fn find_valid_nonce(id: U256, owner: Address) -> u64 {
        (0u64..100_000)
            .find(|nonce| validate_pow(id, owner, *nonce).is_ok())
            .expect("difficulty 1 resolves in ~256 tries")
    }

    #[test]
    fn compute_pow_hash_matches_sha256_of_the_documented_preimage() {
        let id = U256::from(0x1234_5678u64);
        let got = compute_pow_hash(id, OWNER, 42);

        let mut data = id.to_be_bytes::<32>().to_vec();
        data.extend_from_slice(OWNER.as_slice());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&42u64.to_be_bytes());
        let expected = digest(&SHA256, &data);

        assert_eq!(got.as_ref(), expected.as_ref());
    }

    #[test]
    fn a_solution_is_bound_to_its_owner() {
        let id = U256::from(0xABCDu64);
        let nonce = (0u64..100_000)
            .find(|n| validate_pow(id, OWNER, *n).is_ok() && validate_pow(id, OTHER, *n).is_err())
            .expect("a solution that fits only its owner");
        assert!(validate_pow(id, OWNER, nonce).is_ok());
        assert_eq!(
            validate_pow(id, OTHER, nonce),
            Err(PowError::InsufficientProofOfWork)
        );
    }

    #[test]
    fn valid_nonce_passes() {
        let id = U256::from(0xABCDu64);
        assert!(validate_pow(id, OWNER, find_valid_nonce(id, OWNER)).is_ok());
    }

    #[test]
    fn insufficient_pow_is_rejected() {
        let id = U256::from(7u64);
        let nonce = (0u64..100_000)
            .find(|n| compute_pow_hash(id, OWNER, *n)[0] != 0)
            .expect("a failing nonce");
        assert_eq!(
            validate_pow(id, OWNER, nonce),
            Err(PowError::InsufficientProofOfWork)
        );
    }
}
