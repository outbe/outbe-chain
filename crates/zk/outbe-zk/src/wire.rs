//! Generic canonical ZK verifier-wire marshaling.
//!
//! Circuit-specific layouts live next to the generated Noir types.

#![deny(clippy::arithmetic_side_effects)]

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};

use crate::errors::{MalformedAbiReason, NonCanonicalError};

const CANONICAL_ABI_OFFSET: u64 = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProofMarshalingError {
    #[error("zk_verify: input too short ({0} < 64 bytes)")]
    InputTooShort(usize),
    #[error("zk_verify: malformed ABI input ({0})")]
    MalformedAbi(&'static str),
    #[error("zk_verify: combined proof is too short ({0} < 4 bytes)")]
    CombinedProofTooShort(usize),
    #[error("zk_verify: combined proof public input count is {actual}, expected {expected}")]
    WrongPublicInputCount { expected: usize, actual: usize },
    #[error("zk_verify: combined proof public inputs are truncated ({actual} < {expected} bytes)")]
    TruncatedPublicInputs { expected: usize, actual: usize },
    #[error("zk_verify: public input at index {0} is not a canonical BN254 field element")]
    NonCanonicalPublicInput(usize),
    #[error("zk_verify: combined proof length is {actual} bytes, expected {expected}")]
    WrongCombinedProofLength { expected: usize, actual: usize },
}

impl From<MalformedAbiReason> for ProofMarshalingError {
    fn from(reason: MalformedAbiReason) -> Self {
        Self::MalformedAbi(reason.as_str())
    }
}

impl From<ProofMarshalingError> for crate::error::Error {
    fn from(error: ProofMarshalingError) -> Self {
        match error {
            ProofMarshalingError::NonCanonicalPublicInput(_) => {
                NonCanonicalError::PublicInput.into()
            }
            other => Self::Proof(other.to_string()),
        }
    }
}

/// Canonical decoding of `abi.encode(bytes32 circuit_hash, bytes proof)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyCall<'a> {
    pub circuit_hash: [u8; 32],
    pub combined_proof: &'a [u8],
}

pub fn decode_verify_call(input: &[u8]) -> Result<VerifyCall<'_>, ProofMarshalingError> {
    if input.len() < 64 {
        Err(ProofMarshalingError::InputTooShort(input.len()))?;
    }

    let mut circuit_hash = [0u8; 32];
    circuit_hash.copy_from_slice(&input[..32]);

    let offset = read_u64_be_padded(&input[32..64]).ok_or(MalformedAbiReason::OffsetTooLarge)?;
    if offset != CANONICAL_ABI_OFFSET {
        return Err(MalformedAbiReason::NonCanonicalOffset.into());
    }
    let offset = usize::try_from(offset).map_err(|_| MalformedAbiReason::OffsetTooLarge)?;
    let header_end = offset
        .checked_add(32)
        .ok_or(MalformedAbiReason::OffsetOverflow)?;
    if input.len() < header_end {
        return Err(MalformedAbiReason::OffsetPastEnd.into());
    }

    let length =
        read_u64_be_padded(&input[offset..header_end]).ok_or(MalformedAbiReason::LengthTooLarge)?;
    let length = usize::try_from(length).map_err(|_| MalformedAbiReason::LengthTooLarge)?;
    let data_end = header_end
        .checked_add(length)
        .ok_or(MalformedAbiReason::LengthOverflow)?;
    if input.len() < data_end {
        return Err(MalformedAbiReason::PayloadTruncated.into());
    }

    let padded_len = length
        .checked_next_multiple_of(32)
        .ok_or(MalformedAbiReason::PaddingOverflow)?;
    let encoded_end = header_end
        .checked_add(padded_len)
        .ok_or(MalformedAbiReason::PaddingOverflow)?;
    if input.len() != encoded_end {
        return Err(MalformedAbiReason::NonCanonicalTotalLength.into());
    }
    if input[data_end..encoded_end].iter().any(|byte| *byte != 0) {
        return Err(MalformedAbiReason::NonZeroPadding.into());
    }

    Ok(VerifyCall {
        circuit_hash,
        combined_proof: &input[header_end..data_end],
    })
}

/// Decode and validate the public-input words prefixed to a combined proof.
pub fn decode_public_words<const N: usize>(
    combined_proof: &[u8],
    expected_len: usize,
) -> Result<[[u8; 32]; N], ProofMarshalingError> {
    let header = combined_proof
        .get(..4)
        .ok_or(ProofMarshalingError::CombinedProofTooShort(
            combined_proof.len(),
        ))?;
    let count = u32::from_be_bytes(header.try_into().expect("four-byte slice")) as usize;
    if count != N {
        return Err(ProofMarshalingError::WrongPublicInputCount {
            expected: N,
            actual: count,
        });
    }
    let public_end = const { 4 + N * 32 };
    if combined_proof.len() < public_end {
        return Err(ProofMarshalingError::TruncatedPublicInputs {
            expected: public_end,
            actual: combined_proof.len(),
        });
    }
    if combined_proof.len() != expected_len {
        return Err(ProofMarshalingError::WrongCombinedProofLength {
            expected: expected_len,
            actual: combined_proof.len(),
        });
    }

    let mut words = [[0u8; 32]; N];
    let (public_words, remainder) = combined_proof[4..public_end].as_chunks::<32>();
    debug_assert!(remainder.is_empty());
    for (index, word) in public_words.iter().enumerate() {
        words[index] = *word;
        if !is_canonical_field_word(&words[index]) {
            return Err(ProofMarshalingError::NonCanonicalPublicInput(index));
        }
    }
    Ok(words)
}

/// Decode a right-aligned `u64` from one 32-byte ABI word.
pub fn read_u64_be_padded(slot: &[u8]) -> Option<u64> {
    if slot.len() != 32 || slot[..24].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(u64::from_be_bytes(slot[24..].try_into().ok()?))
}

/// Decode a right-aligned `u128` from one 32-byte ABI word.
pub fn read_u128_be_padded(slot: &[u8]) -> Option<u128> {
    if slot.len() != 32 || slot[..16].iter().any(|byte| *byte != 0) {
        return None;
    }
    Some(u128::from_be_bytes(slot[16..].try_into().ok()?))
}

fn is_canonical_field_word(word: &[u8; 32]) -> bool {
    let field = Fr::from_be_bytes_mod_order(word);
    let bytes = field.into_bigint().to_bytes_be();
    let mut canonical = [0u8; 32];
    let Some(start) = 32usize.checked_sub(bytes.len()) else {
        return false;
    };
    canonical[start..].copy_from_slice(&bytes);
    canonical == *word
}

#[cfg(test)]
mod test_support {
    pub(crate) fn u64_word(value: u64) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[24..].copy_from_slice(&value.to_be_bytes());
        word
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use test_support::u64_word;

    fn abi_encode(circuit_hash: [u8; 32], proof: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(96 + proof.len() + 31);
        out.extend_from_slice(&circuit_hash);
        out.extend_from_slice(&u64_word(64));
        out.extend_from_slice(&u64_word(proof.len() as u64));
        out.extend_from_slice(proof);
        out.resize(out.len() + (32 - proof.len() % 32) % 32, 0);
        out
    }

    fn boundary_u64_strategy() -> impl Strategy<Value = u64> {
        prop_oneof![
            Just(0u64),
            Just(1),
            Just(31),
            Just(32),
            Just(33),
            Just(63),
            Just(64),
            Just(65),
            Just(95),
            Just(96),
            (u64::from(u32::MAX) - 1)..=(u64::from(u32::MAX) + 1),
            (u64::MAX - 128)..=u64::MAX,
            any::<u64>(),
        ]
    }

    fn boundary_word_strategy() -> impl Strategy<Value = [u8; 32]> {
        prop_oneof![
            boundary_u64_strategy().prop_map(u64_word),
            (0usize..24, 1u8..=u8::MAX, boundary_u64_strategy()).prop_map(
                |(hi_idx, hi_byte, lo)| {
                    let mut word = u64_word(lo);
                    word[hi_idx] = hi_byte;
                    word
                }
            ),
        ]
    }

    #[test]
    fn verify_call_decodes_canonical_solidity_abi() {
        let hash = [0xabu8; 32];
        let proof = [0xcdu8; 35];
        let encoded = abi_encode(hash, &proof);
        let decoded = decode_verify_call(&encoded).unwrap();
        assert_eq!(decoded.circuit_hash, hash);
        assert_eq!(decoded.combined_proof, proof);
    }

    #[test]
    fn verify_call_rejects_short_and_non_canonical_offsets() {
        assert_eq!(
            decode_verify_call(&[0u8; 32]),
            Err(ProofMarshalingError::InputTooShort(32))
        );
        for offset in [65u64, 96, u64::MAX - 31, u64::MAX - 15, u64::MAX] {
            let mut encoded = abi_encode([0; 32], &[0; 32]);
            encoded[56..64].copy_from_slice(&offset.to_be_bytes());
            assert_eq!(
                decode_verify_call(&encoded),
                Err(ProofMarshalingError::MalformedAbi("non-canonical offset"))
            );
        }

        let mut oversized_offset = abi_encode([0; 32], &[0; 32]);
        oversized_offset[32] = 1;
        assert_eq!(
            decode_verify_call(&oversized_offset),
            Err(ProofMarshalingError::MalformedAbi("offset too large"))
        );
    }

    #[test]
    fn verify_call_rejects_overflowing_and_oversized_lengths() {
        let mut oversized_length = abi_encode([0; 32], &[0; 32]);
        oversized_length[64] = 1;
        assert_eq!(
            decode_verify_call(&oversized_length),
            Err(ProofMarshalingError::MalformedAbi("length too large"))
        );

        #[cfg(target_pointer_width = "64")]
        {
            for length in [u64::MAX - 95, u64::MAX - 31, u64::MAX - 15, u64::MAX] {
                let mut encoded = abi_encode([0; 32], &[0; 32]);
                encoded[88..96].copy_from_slice(&length.to_be_bytes());
                assert_eq!(
                    decode_verify_call(&encoded),
                    Err(ProofMarshalingError::MalformedAbi("length overflow"))
                );
            }

            let mut max_in_range = abi_encode([0; 32], &[0; 32]);
            max_in_range[88..96].copy_from_slice(&(u64::MAX - 96).to_be_bytes());
            assert_eq!(
                decode_verify_call(&max_in_range),
                Err(ProofMarshalingError::MalformedAbi("payload truncated"))
            );
        }
    }

    #[test]
    fn verify_call_rejects_truncation_padding_and_trailing_bytes() {
        let mut truncated = abi_encode([0; 32], &[0xcd; 35]);
        truncated.truncate(100);
        assert_eq!(
            decode_verify_call(&truncated),
            Err(ProofMarshalingError::MalformedAbi("payload truncated"))
        );

        let mut non_zero_padding = abi_encode([0; 32], &[0xcd; 35]);
        *non_zero_padding.last_mut().unwrap() = 1;
        assert_eq!(
            decode_verify_call(&non_zero_padding),
            Err(ProofMarshalingError::MalformedAbi("non-zero padding"))
        );

        let mut trailing = abi_encode([0; 32], &[0xcd; 32]);
        trailing.extend_from_slice(&[0; 32]);
        assert_eq!(
            decode_verify_call(&trailing),
            Err(ProofMarshalingError::MalformedAbi(
                "non-canonical total length"
            ))
        );
    }

    #[test]
    fn marshaling_errors_keep_chain_visible_text() {
        let cases = [
            (
                ProofMarshalingError::InputTooShort(3),
                "zk_verify: input too short (3 < 64 bytes)",
            ),
            (
                ProofMarshalingError::MalformedAbi("bad offset"),
                "zk_verify: malformed ABI input (bad offset)",
            ),
            (
                ProofMarshalingError::CombinedProofTooShort(2),
                "zk_verify: combined proof is too short (2 < 4 bytes)",
            ),
            (
                ProofMarshalingError::WrongPublicInputCount {
                    expected: 9,
                    actual: 8,
                },
                "zk_verify: combined proof public input count is 8, expected 9",
            ),
            (
                ProofMarshalingError::TruncatedPublicInputs {
                    expected: 292,
                    actual: 100,
                },
                "zk_verify: combined proof public inputs are truncated (100 < 292 bytes)",
            ),
            (
                ProofMarshalingError::NonCanonicalPublicInput(5),
                "zk_verify: public input at index 5 is not a canonical BN254 field element",
            ),
            (
                ProofMarshalingError::WrongCombinedProofLength {
                    expected: 8_292,
                    actual: 8_260,
                },
                "zk_verify: combined proof length is 8260 bytes, expected 8292",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
        }
    }

    proptest! {
        #[test]
        fn proptest_decode_verify_call_never_panics_on_arbitrary_bytes(
            raw in proptest::collection::vec(any::<u8>(), 0..=512),
        ) {
            if let Ok(decoded) = decode_verify_call(&raw) {
                prop_assert_eq!(abi_encode(decoded.circuit_hash, decoded.combined_proof), raw);
            }
        }

        #[test]
        fn proptest_decode_verify_call_rejects_mutated_offset_and_length_words(
            circuit_hash in any::<[u8; 32]>(),
            proof in proptest::collection::vec(any::<u8>(), 0..=160),
            mutate_length_word in any::<bool>(),
            replacement in boundary_word_strategy(),
        ) {
            let mut encoded = abi_encode(circuit_hash, &proof);
            let slot_range = if mutate_length_word { 64..96 } else { 32..64 };
            encoded[slot_range].copy_from_slice(&replacement);

            let result = decode_verify_call(&encoded);
            let payload_cap = encoded.len() - 96;
            let valid_len = read_u64_be_padded(&encoded[64..96])
                .and_then(|len| usize::try_from(len).ok())
                .filter(|&len| {
                    let padded = len.div_ceil(32).checked_mul(32);
                    padded == Some(payload_cap)
                        && encoded[96 + len..].iter().all(|&byte| byte == 0)
                });
            let expected_ok = encoded[32..64] == u64_word(64) && valid_len.is_some();

            if expected_ok {
                let len = valid_len.unwrap();
                let decoded = result.expect("canonical encoding must decode");
                prop_assert_eq!(decoded.circuit_hash, circuit_hash);
                prop_assert_eq!(decoded.combined_proof, &encoded[96..96 + len]);
            } else {
                prop_assert!(result.is_err());
            }
        }
    }
}
