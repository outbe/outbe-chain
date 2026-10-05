//! Shared proof-of-work gate for entity mining.
//!
//! [`POW_DIFFICULTY`] and [`meets_difficulty`] are the protocol's verdict on a
//! work hash, whatever preimage produced it.
//!
//! Nod and Gem hash
//! `domain_tag || right_id_be32 || owner_20 || mining_sequence_be8 || nonce_be8` with
//! [`SINGLE_EXERCISE_SEQUENCE`]. Intex builds its own preimage (owner, amount,
//! series, sequence, nonce) and only reuses the difficulty check.

use alloy_primitives::{Address, U256};
use ring::digest::{digest, SHA256};

/// PoW difficulty: number of leading zero bytes required in the SHA256 hash.
/// Identical across all entity factories.
pub const POW_DIFFICULTY: usize = 1;

/// Mining sequence for a right that is exercised once (Nod and Gem).
pub const SINGLE_EXERCISE_SEQUENCE: u64 = 0;

/// Length every mining domain tag shares, which keeps the preimage a fixed array.
pub const MINING_DOMAIN_TAG_LEN: usize = 19;

const MINING_PREIMAGE_LEN: usize = MINING_DOMAIN_TAG_LEN + 68;

/// Right family a mining preimage belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiningDomain {
    Nod,
    Gem,
}

impl MiningDomain {
    /// Hashed ahead of the body, so a nonce solved for one family cannot be replayed
    /// against another. Changing a tag invalidates every nonce of that family.
    pub const fn tag(self) -> &'static [u8; MINING_DOMAIN_TAG_LEN] {
        match self {
            Self::Nod => b"OUTBE_NOD_MINING_V1",
            Self::Gem => b"OUTBE_GEM_MINING_V1",
        }
    }
}

/// Proof-of-work failure modes. Factories map these onto their own error enums;
/// kept exhaustive so a new variant forces every mapping site to handle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowError {
    /// The computed hash does not have [`POW_DIFFICULTY`] leading zero bytes.
    InsufficientProofOfWork,
}

/// SHA256 over `domain_tag || right_id_be32 || owner_20 || mining_sequence_be8 || nonce_be8`.
pub fn compute_mining_pow_hash(
    domain: MiningDomain,
    right_id: U256,
    owner: Address,
    mining_sequence: u64,
    nonce: u64,
) -> [u8; 32] {
    const TAG: usize = MINING_DOMAIN_TAG_LEN;
    let mut data = [0u8; MINING_PREIMAGE_LEN];
    data[..TAG].copy_from_slice(domain.tag());
    data[TAG..TAG + 32].copy_from_slice(&right_id.to_be_bytes::<32>());
    data[TAG + 32..TAG + 52].copy_from_slice(owner.as_slice());
    data[TAG + 52..TAG + 60].copy_from_slice(&mining_sequence.to_be_bytes());
    data[TAG + 60..].copy_from_slice(&nonce.to_be_bytes());
    let digest = digest(&SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Validates that [`compute_mining_pow_hash`] has [`POW_DIFFICULTY`] leading
/// zero bytes.
pub fn validate_mining_pow(
    domain: MiningDomain,
    right_id: U256,
    owner: Address,
    mining_sequence: u64,
    nonce: u64,
) -> Result<(), PowError> {
    meets_difficulty(&compute_mining_pow_hash(
        domain,
        right_id,
        owner,
        mining_sequence,
        nonce,
    ))
}

/// The protocol's single verdict on a work hash, whatever preimage produced it:
/// a right whose own scheme differs still weighs its work here.
pub fn meets_difficulty(hash: &[u8; 32]) -> Result<(), PowError> {
    if hash[..POW_DIFFICULTY].iter().any(|byte| *byte != 0) {
        return Err(PowError::InsufficientProofOfWork);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force the lowest nonce that satisfies the mining scheme for the
    /// current `POW_DIFFICULTY`. With difficulty=1 the expected loop length is
    /// ~256 iterations.
    fn find_valid_mining_nonce(domain: MiningDomain, right_id: U256, owner: Address) -> u64 {
        for nonce in 0u64..100_000 {
            if validate_mining_pow(domain, right_id, owner, SINGLE_EXERCISE_SEQUENCE, nonce).is_ok()
            {
                return nonce;
            }
        }
        panic!("no valid mining nonce found in 100k attempts")
    }

    #[test]
    fn mining_pow_hash_is_domain_right_id_owner_sequence_and_nonce() {
        let right_id = U256::from(0x1234_5678u64);
        let owner = Address::repeat_byte(0x11);
        let got = compute_mining_pow_hash(
            MiningDomain::Nod,
            right_id,
            owner,
            SINGLE_EXERCISE_SEQUENCE,
            42,
        );

        let mut data = MiningDomain::Nod.tag().to_vec();
        data.extend_from_slice(&right_id.to_be_bytes::<32>());
        data.extend_from_slice(owner.as_slice());
        data.extend_from_slice(&SINGLE_EXERCISE_SEQUENCE.to_be_bytes());
        data.extend_from_slice(&42u64.to_be_bytes());
        let expected = digest(&SHA256, &data);

        assert_eq!(got.as_ref(), expected.as_ref());
        assert_ne!(
            got,
            compute_mining_pow_hash(
                MiningDomain::Nod,
                right_id,
                Address::repeat_byte(0x22),
                SINGLE_EXERCISE_SEQUENCE,
                42
            )
        );
        assert_ne!(
            got,
            compute_mining_pow_hash(MiningDomain::Nod, right_id, owner, 1, 42)
        );
    }

    /// Cross-language vectors: an external miner must reproduce these digests byte for byte.
    #[test]
    fn mining_pow_golden_vectors() {
        let right_id = U256::from(0x1234_5678u64);
        let owner = Address::repeat_byte(0x11);
        for (domain, tag, expected) in [
            (
                MiningDomain::Nod,
                "OUTBE_NOD_MINING_V1",
                "600ce7d86d05c5587e7c77055f0942e499e7b2a4a0ae9a2252d14c31dd7750f4",
            ),
            (
                MiningDomain::Gem,
                "OUTBE_GEM_MINING_V1",
                "d260e69b7940b333aa6a5beb27cf8ea3c6a760cfc6d7e5c1a1a281de65278979",
            ),
        ] {
            assert_eq!(domain.tag().as_slice(), tag.as_bytes());
            assert_eq!(
                alloy_primitives::hex::encode(compute_mining_pow_hash(
                    domain,
                    right_id,
                    owner,
                    SINGLE_EXERCISE_SEQUENCE,
                    42
                )),
                expected
            );
        }
    }

    #[test]
    fn a_nonce_solved_for_one_domain_does_not_carry_to_the_other() {
        let right_id = U256::from(0x5EEDu64);
        let owner = Address::repeat_byte(0x66);
        assert_ne!(
            compute_mining_pow_hash(
                MiningDomain::Nod,
                right_id,
                owner,
                SINGLE_EXERCISE_SEQUENCE,
                7
            ),
            compute_mining_pow_hash(
                MiningDomain::Gem,
                right_id,
                owner,
                SINGLE_EXERCISE_SEQUENCE,
                7
            )
        );
        let nod_nonce = find_valid_mining_nonce(MiningDomain::Nod, right_id, owner);
        let gem_nonce = find_valid_mining_nonce(MiningDomain::Gem, right_id, owner);
        assert_ne!(
            nod_nonce, gem_nonce,
            "the fixture must exercise two distinct solutions"
        );
        assert!(validate_mining_pow(
            MiningDomain::Gem,
            right_id,
            owner,
            SINGLE_EXERCISE_SEQUENCE,
            nod_nonce
        )
        .is_err());
    }

    #[test]
    fn mining_pow_accepts_a_solved_nonce_for_that_owner() {
        let right_id = U256::from(0xABCDu64);
        let owner = Address::repeat_byte(0x33);
        let other = Address::repeat_byte(0x44);
        let nonce = find_valid_mining_nonce(MiningDomain::Nod, right_id, owner);
        assert!(validate_mining_pow(
            MiningDomain::Nod,
            right_id,
            owner,
            SINGLE_EXERCISE_SEQUENCE,
            nonce
        )
        .is_ok());
        assert_ne!(
            compute_mining_pow_hash(
                MiningDomain::Nod,
                right_id,
                owner,
                SINGLE_EXERCISE_SEQUENCE,
                nonce
            ),
            compute_mining_pow_hash(
                MiningDomain::Nod,
                right_id,
                other,
                SINGLE_EXERCISE_SEQUENCE,
                nonce
            )
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
        let right_id = U256::from(7u64);
        let owner = Address::repeat_byte(0x55);
        // Find a nonce whose first byte is non-zero (fails difficulty=1).
        for nonce in 0u64..100_000 {
            if compute_mining_pow_hash(
                MiningDomain::Nod,
                right_id,
                owner,
                SINGLE_EXERCISE_SEQUENCE,
                nonce,
            )[0] != 0
            {
                assert_eq!(
                    validate_mining_pow(
                        MiningDomain::Nod,
                        right_id,
                        owner,
                        SINGLE_EXERCISE_SEQUENCE,
                        nonce
                    ),
                    Err(PowError::InsufficientProofOfWork)
                );
                return;
            }
        }
        panic!("no failing nonce found");
    }

    /// Known answers for external miners, computed outside this implementation.
    #[test]
    fn mining_pow_matches_the_known_answer_vectors() {
        use alloy_primitives::{address, b256, B256};

        assert_eq!(POW_DIFFICULTY, 1);
        let wide_id = b256!("0xffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100");
        let vectors = [
            (
                MiningDomain::Nod,
                U256::from(0x1234_5678u64),
                address!("0x1111111111111111111111111111111111111111"),
                42,
                b256!("0x600ce7d86d05c5587e7c77055f0942e499e7b2a4a0ae9a2252d14c31dd7750f4"),
                556,
                b256!("0x004a9bab5e835bb0a7ec4d52cc0e94be2dd91406769538b4204677c737ef136d"),
            ),
            (
                MiningDomain::Gem,
                U256::from(0x1234_5678u64),
                address!("0x1111111111111111111111111111111111111111"),
                42,
                b256!("0xd260e69b7940b333aa6a5beb27cf8ea3c6a760cfc6d7e5c1a1a281de65278979"),
                314,
                b256!("0x006f6f7c0bf0e706ec50554404806df0810f7230d85fc65c5e55403778b0741a"),
            ),
            (
                MiningDomain::Nod,
                U256::from_be_bytes(wide_id.0),
                address!("0xabcdef0123456789abcdef0123456789abcdef01"),
                0x0102_0304_0506_0708,
                b256!("0x8cf7cc1ac26dd020c313013c98bb08ab8253fe31045e253e2a357ac461ac3330"),
                248,
                b256!("0x007c65e12dd39ec397504f1c92b9949733a8a4921cddf5820e802e1c16a4e3c2"),
            ),
            (
                MiningDomain::Gem,
                U256::from(1u64),
                address!("0x7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a"),
                0,
                b256!("0x934e259014c8059c60179c9306e8a9aa73a846cd059fca8f23b2bb898ae10fb8"),
                299,
                b256!("0x00693450a64179eecb12473fc4f8006bf2373718e7d0de26a1eb59de7c2c4c5c"),
            ),
        ];
        for (domain, right_id, owner, nonce, hash, first_valid, first_valid_hash) in vectors {
            let pow =
                |nonce: u64| B256::from(compute_mining_pow_hash(domain, right_id, owner, 0, nonce));
            assert_eq!(pow(nonce), hash);
            assert_eq!(pow(first_valid), first_valid_hash);
            assert!(validate_mining_pow(domain, right_id, owner, 0, first_valid).is_ok());
            assert!((0..first_valid)
                .all(|nonce| validate_mining_pow(domain, right_id, owner, 0, nonce).is_err()));
        }
    }
}
