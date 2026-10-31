//! Legacy participation bitmap codec (the "OUTB" format).
//!
//! The codec encodes which validators participated in a block's finalization
//! as a compact bitmap. No production path writes this format into block
//! `extra_data`. Participation travels in the Phase 1
//! [`crate::consensus_metadata::CertifiedParentAccountingMetadata`] system tx,
//! and `extra_data` carries `OutbeBlockArtifacts`. Only tests use this
//! encoder. The crate has no decoder for this format.
//!
//! # Format
//!
//! ```text
//! [4 bytes: magic "OUTB"]
//! [1 byte:  version = 0x01]
//! [2 bytes: validator_count (big-endian u16)]
//! [ceil(validator_count / 8) bytes: signer bitmap]
//! [1 byte:  missed_proposer_count]
//! [20 * missed_proposer_count bytes: missed proposer addresses]
//! [1 byte:  byzantine_count]
//! [20 * byzantine_count bytes: byzantine validator addresses]
//! ```
//!
//! For 128 validators with 0 missed and 0 byzantine: 4 + 1 + 2 + 16 + 1 + 1 = 25 bytes.

use alloy_primitives::{Address, Bytes};

use crate::consensus::OUTBE_MAX_EXTRA_DATA_SIZE;
#[cfg(test)]
use crate::error::PrecompileError;
use crate::error::{ParticipationError, Result};

/// Magic bytes identifying Outbe participation data.
const MAGIC: &[u8; 4] = b"OUTB";
/// Current encoding version.
const VERSION: u8 = 0x01;

/// Encode participation bitmap into `extra_data` bytes (no missed proposers or byzantine).
pub fn encode_participation(validators: &[Address], signers: &[bool]) -> Result<Bytes> {
    encode_participation_extended(validators, signers, &[], &[])
}

/// Encode participation bitmap, missed proposer addresses, and byzantine validator
/// addresses into `extra_data` bytes.
pub fn encode_participation_extended(
    validators: &[Address],
    signers: &[bool],
    missed_proposers: &[Address],
    byzantine_validators: &[Address],
) -> Result<Bytes> {
    encode_participation_extended_with_limit(
        validators,
        signers,
        missed_proposers,
        byzantine_validators,
        OUTBE_MAX_EXTRA_DATA_SIZE,
    )
}

/// Encode participation metadata with an explicit byte budget.
pub fn encode_participation_extended_with_limit(
    validators: &[Address],
    signers: &[bool],
    missed_proposers: &[Address],
    byzantine_validators: &[Address],
    max_extra_data_size: usize,
) -> Result<Bytes> {
    // Return structured error instead of panicking on length mismatch.
    if validators.len() != signers.len() {
        return Err(ParticipationError::ValidatorsSignersLengthMismatch {
            validators_len: validators.len(),
            signers_len: signers.len(),
        }
        .into());
    }

    if validators.len() > u16::MAX as usize {
        return Err(ParticipationError::ValidatorSetTooLarge {
            len: validators.len(),
        }
        .into());
    }

    if missed_proposers.len() > u8::MAX as usize {
        return Err(ParticipationError::MissedProposerListTooLarge {
            len: missed_proposers.len(),
        }
        .into());
    }

    if byzantine_validators.len() > u8::MAX as usize {
        return Err(ParticipationError::ByzantineValidatorListTooLarge {
            len: byzantine_validators.len(),
        }
        .into());
    }

    let count = validators.len() as u16;
    let bitmap_len = (count as usize).div_ceil(8);
    let header_len = 4 + 1 + 2 + bitmap_len + 1 + 1; // magic + ver + count + bitmap + 2 count bytes
    let total_len = header_len + (missed_proposers.len() * 20) + (byzantine_validators.len() * 20);

    if total_len > max_extra_data_size {
        return Err(ParticipationError::ExceedsExtraDataBudget {
            total_len,
            max_extra_data_size,
        }
        .into());
    }

    let mut buf = Vec::with_capacity(total_len);

    // Magic
    buf.extend_from_slice(MAGIC);
    // Version
    buf.push(VERSION);
    // Validator count (big-endian)
    buf.extend_from_slice(&count.to_be_bytes());
    // Bitmap
    let mut bitmap = vec![0u8; bitmap_len];
    for (i, &signed) in signers.iter().enumerate() {
        if signed {
            bitmap[i / 8] |= 1 << (7 - (i % 8));
        }
    }
    buf.extend_from_slice(&bitmap);
    // Missed proposer count + addresses
    buf.push(missed_proposers.len() as u8);
    for addr in missed_proposers {
        buf.extend_from_slice(addr.as_slice());
    }
    // Byzantine validator count + addresses
    buf.push(byzantine_validators.len() as u8);
    for addr in byzantine_validators {
        buf.extend_from_slice(addr.as_slice());
    }

    Ok(Bytes::from(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_participation_matches_wire_vector() {
        let validators: Vec<Address> = (0..4).map(Address::with_last_byte).collect();
        let signers = vec![true, false, true, false];

        let encoded = encode_participation(&validators, &signers).unwrap();

        assert_eq!(encoded.as_ref(), b"OUTB\x01\x00\x04\xa0\x00\x00");
    }

    #[test]
    fn extended_sections_follow_the_bitmap_in_order() {
        let validators = [Address::with_last_byte(2), Address::with_last_byte(1)];
        let missed = Address::with_last_byte(0xAA);
        let byzantine = Address::with_last_byte(0xBB);

        let encoded =
            encode_participation_extended(&validators, &[true, false], &[missed], &[byzantine])
                .unwrap();

        let mut expected = b"OUTB\x01\x00\x02\x80".to_vec();
        expected.push(1);
        expected.extend_from_slice(missed.as_slice());
        expected.push(1);
        expected.extend_from_slice(byzantine.as_slice());
        assert_eq!(encoded.as_ref(), expected.as_slice());
    }

    #[test]
    fn test_128_validators_with_missed() {
        let validators: Vec<Address> = (0..128u8).map(Address::with_last_byte).collect();
        let mut signers = vec![true; 128];
        signers[0] = false;
        signers[63] = false;
        signers[127] = false;

        let missed = vec![Address::with_last_byte(0xFF)];

        let encoded = encode_participation_extended(&validators, &signers, &missed, &[]).unwrap();
        // 4 + 1 + 2 + 16 + 1 + 20 + 1(byzantine count=0) = 45 bytes
        assert_eq!(encoded.len(), 45);
        let bitmap = &encoded[7..23];
        assert_eq!(bitmap[0], 0b0111_1111);
        assert_eq!(bitmap[7], 0b1111_1110);
        assert_eq!(bitmap[15], 0b1111_1110);
        assert!(bitmap[1..7]
            .iter()
            .chain(&bitmap[8..15])
            .all(|&b| b == 0xFF));
        assert_eq!(encoded[23], 1);
        assert_eq!(&encoded[24..44], missed[0].as_slice());
        assert_eq!(encoded[44], 0);
    }

    /// Length mismatch returns error instead of panicking.
    #[test]
    fn test_encode_length_mismatch_returns_error() {
        let validators: Vec<Address> = (0..4).map(Address::with_last_byte).collect();
        let signers = vec![true, true]; // 2 signers for 4 validators

        let result = encode_participation(&validators, &signers);
        assert!(
            result.is_err(),
            "length mismatch must return error, not panic"
        );
    }

    #[test]
    fn test_empty_participation_encodes_minimal_header() {
        let encoded = encode_participation_extended(&[], &[], &[], &[]).unwrap();

        assert_eq!(encoded.as_ref(), b"OUTB\x01\x00\x00\x00\x00");
    }

    #[test]
    fn test_encode_with_limit_returns_error_on_overflow() {
        let validators: Vec<Address> = (0..4).map(Address::with_last_byte).collect();
        let signers = vec![true, true, true, true];
        let missed = vec![Address::with_last_byte(0xAA); 2];

        let err = encode_participation_extended_with_limit(&validators, &signers, &missed, &[], 40)
            .unwrap_err();

        assert!(matches!(err, PrecompileError::Revert(_)));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating a validator set of size 1..=128.
    fn validators_and_signers() -> impl Strategy<Value = (Vec<Address>, Vec<bool>)> {
        (1..=128usize).prop_flat_map(|n| {
            let addrs = proptest::collection::vec(any::<[u8; 20]>().prop_map(Address::from), n);
            let signers = proptest::collection::vec(any::<bool>(), n);
            (addrs, signers)
        })
    }

    /// Strategy for generating missed proposer addresses (0..=10).
    fn missed_proposers() -> impl Strategy<Value = Vec<Address>> {
        proptest::collection::vec(any::<[u8; 20]>().prop_map(Address::from), 0..=10)
    }

    proptest! {
        /// The encoded length follows the format header and section counts.
        #[test]
        fn encoded_length_follows_format(
            (validators, signers) in validators_and_signers(),
            missed in missed_proposers(),
        ) {
            let encoded = encode_participation_extended(&validators, &signers, &missed, &[]).unwrap();
            let bitmap_len = validators.len().div_ceil(8);
            prop_assert_eq!(encoded.len(), 4 + 1 + 2 + bitmap_len + 1 + missed.len() * 20 + 1);
            prop_assert_eq!(usize::from(encoded[7 + bitmap_len]), missed.len());
        }

        /// Encoding is deterministic. Same inputs always produce identical bytes.
        #[test]
        fn encoding_deterministic(
            (validators, signers) in validators_and_signers(),
            missed in missed_proposers(),
        ) {
            let a = encode_participation_extended(&validators, &signers, &missed, &[]).unwrap();
            let b = encode_participation_extended(&validators, &signers, &missed, &[]).unwrap();
            prop_assert_eq!(a, b);
        }
    }
}
