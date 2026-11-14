use super::*;
use alloy_sol_types::SolError;
use proptest::prelude::*;

fn usize_word(value: usize) -> [u8; 32] {
    let mut word = [0u8; 32];
    let bytes = value.to_be_bytes();
    word[32 - bytes.len()..].copy_from_slice(&bytes);
    word
}

fn renewal_call(node_len: usize, enclave_len: usize) -> Vec<u8> {
    ITeeRegistryV1::renewEnclaveCall {
        evidence: vec![1, 2, 3].into(),
        nodeSignature: vec![0x51; node_len].into(),
        enclaveSignature: vec![0x52; enclave_len].into(),
    }
    .abi_encode()
}

fn register_call(binding_len: usize, validator_len: usize, node_len: usize) -> Vec<u8> {
    ITeeRegistryV1::registerEnclaveCall {
        evidence: vec![1, 2, 3].into(),
        nodeSignature: vec![0x51; 65].into(),
        enclaveSignature: vec![0x52; 64].into(),
        validatorNodeBinding: vec![0x53; binding_len].into(),
        validatorSignature: vec![0x54; validator_len].into(),
        nodeBindingSignature: vec![0x55; node_len].into(),
    }
    .abi_encode()
}

fn valid_calls() -> [Vec<u8>; 2] {
    [
        renewal_call(65, 64),
        register_call(ValidatorNodeBindingV1::CANONICAL_LEN, 65, 65),
    ]
}

fn assert_revert(encoded: &[u8], expected: &[u8]) {
    let error = preflight_evidence_mutator_call(encoded).err();
    assert!(
        matches!(&error, Some(PrecompileError::RevertBytes(bytes)) if bytes.as_ref() == expected),
        "expected revert bytes {expected:02x?}, got {error:?}"
    );
}

#[test]
fn initial_offsets_are_decoded_before_checking_evidence_offset() {
    for mut encoded in valid_calls() {
        encoded[35] = 0;
        encoded[36] = 1;
        assert_revert(
            &encoded,
            &ITeeRegistryV1::AbiIntegerExceedsHostUsize {}.abi_encode(),
        );
    }
}

#[test]
fn common_offsets_and_signature_lengths_keep_their_exact_errors() {
    for encoded in valid_calls() {
        for (word, expected) in [
            (
                0,
                ITeeRegistryV1::NonCanonicalEvidenceOffset {}.abi_encode(),
            ),
            (
                1,
                ITeeRegistryV1::NonCanonicalNodeSignatureOffset {}.abi_encode(),
            ),
            (
                2,
                ITeeRegistryV1::NonCanonicalEnclaveSignatureOffset {}.abi_encode(),
            ),
        ] {
            let mut changed = encoded.clone();
            changed[4 + word * 32..4 + (word + 1) * 32].fill(0);
            assert_revert(&changed, &expected);
        }
    }
    assert_revert(
        &renewal_call(64, 64),
        &ITeeRegistryV1::NodePopSignatureLengthInvalid {}.abi_encode(),
    );
    assert_revert(
        &renewal_call(65, 63),
        &ITeeRegistryV1::EnclavePopSignatureLengthInvalid {}.abi_encode(),
    );
}

#[test]
fn validator_offsets_are_decoded_before_checking_binding_offset() {
    let mut encoded = register_call(ValidatorNodeBindingV1::CANONICAL_LEN, 65, 65);
    encoded[100..132].fill(0);
    encoded[132] = 1;
    assert_revert(
        &encoded,
        &ITeeRegistryV1::AbiIntegerExceedsHostUsize {}.abi_encode(),
    );
}

#[test]
fn validator_binding_and_signature_lengths_keep_their_exact_errors() {
    for (binding_len, validator_len, node_len, expected) in [
        (
            ValidatorNodeBindingV1::CANONICAL_LEN - 1,
            65,
            65,
            ITeeRegistryV1::ValidatorNodeBindingLengthInvalid {
                value: u64::try_from(ValidatorNodeBindingV1::CANONICAL_LEN).unwrap(),
            }
            .abi_encode(),
        ),
        (
            ValidatorNodeBindingV1::CANONICAL_LEN,
            64,
            65,
            ITeeRegistryV1::ValidatorBindingSignatureLengthInvalid {}.abi_encode(),
        ),
        (
            ValidatorNodeBindingV1::CANONICAL_LEN,
            65,
            64,
            ITeeRegistryV1::NodeBindingSignatureLengthInvalid {}.abi_encode(),
        ),
    ] {
        assert_revert(
            &register_call(binding_len, validator_len, node_len),
            &expected,
        );
    }
}

#[test]
fn padding_precedes_trailing_bytes_and_missing_heads_keep_their_errors() {
    for encoded in valid_calls() {
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_revert(&trailing, &ITeeRegistryV1::TrailingAbiBytes {}.abi_encode());
        let mut padding = trailing;
        let evidence_offset = U256::from_be_slice(&encoded[4..36]).to::<usize>();
        padding[4 + evidence_offset + 32 + 3] = 1;
        assert_revert(
            &padding,
            &ITeeRegistryV1::NonZeroDynamicPadding {}.abi_encode(),
        );
    }
    assert_revert(
        &[0, 0, 0],
        &ITeeRegistryV1::MissingFunctionSelector {}.abi_encode(),
    );
    assert_revert(
        &[0, 0, 0, 0],
        &ITeeRegistryV1::TruncatedArgumentHead {}.abi_encode(),
    );
}

#[test]
fn dynamic_length_overflow_and_oversized_words_keep_their_exact_errors() {
    let mut oversized_len = renewal_call(65, 64);
    oversized_len[100] = 1;
    assert_revert(
        &oversized_len,
        &ITeeRegistryV1::AbiIntegerExceedsHostUsize {}.abi_encode(),
    );

    for length in [
        usize::MAX - 127,
        usize::MAX - 31,
        usize::MAX - 15,
        usize::MAX,
    ] {
        let mut overflow = renewal_call(65, 64);
        overflow[100..132].copy_from_slice(&usize_word(length));
        assert_revert(
            &overflow,
            &ITeeRegistryV1::DynamicLengthOverflow {}.abi_encode(),
        );
    }

    let mut truncated_max = renewal_call(65, 64);
    truncated_max[100..132].copy_from_slice(&usize_word(usize::MAX - 128));
    assert_revert(
        &truncated_max,
        &ITeeRegistryV1::TruncatedDynamicValue {}.abi_encode(),
    );
}

fn canonical_preflight_reencode(selector: [u8; 4], preflight: &RegisterPreflight<'_>) -> Vec<u8> {
    if selector == ITeeRegistryV1::registerEnclaveCall::SELECTOR {
        return ITeeRegistryV1::registerEnclaveCall {
            evidence: preflight.evidence.to_vec().into(),
            nodeSignature: preflight.node_signature.to_vec().into(),
            enclaveSignature: preflight.enclave_signature.to_vec().into(),
            validatorNodeBinding: preflight.validator_node_binding.unwrap().to_vec().into(),
            validatorSignature: preflight.validator_signature.unwrap().to_vec().into(),
            nodeBindingSignature: preflight.node_binding_signature.unwrap().to_vec().into(),
        }
        .abi_encode();
    }
    let mut encoded = ITeeRegistryV1::renewEnclaveCall {
        evidence: preflight.evidence.to_vec().into(),
        nodeSignature: preflight.node_signature.to_vec().into(),
        enclaveSignature: preflight.enclave_signature.to_vec().into(),
    }
    .abi_encode();
    encoded[..4].copy_from_slice(&selector);
    encoded
}

fn teeregistry_word_strategy() -> impl Strategy<Value = [u8; 32]> {
    let scalar = prop_oneof![
        Just(0usize),
        Just(31),
        Just(32),
        Just(64),
        Just(65),
        Just(96),
        Just(192),
        Just(usize::MAX & !31),
        (usize::MAX - 256)..=usize::MAX,
        any::<usize>(),
    ];
    prop_oneof![
        scalar.clone().prop_map(usize_word),
        (
            0usize..(32 - core::mem::size_of::<usize>()),
            1u8..=u8::MAX,
            scalar
        )
            .prop_map(|(hi_idx, hi_byte, lo)| {
                let mut word = usize_word(lo);
                word[hi_idx] = hi_byte;
                word
            }),
    ]
}

proptest! {
    #[test]
    fn proptest_preflight_never_panics_on_arbitrary_bytes(
        selector_mode in 0u8..3,
        mut raw in proptest::collection::vec(any::<u8>(), 0..=512),
    ) {
        if raw.len() >= 4 {
            match selector_mode {
                1 => raw[..4].copy_from_slice(&ITeeRegistryV1::registerEnclaveCall::SELECTOR),
                2 => raw[..4].copy_from_slice(&ITeeRegistryV1::renewEnclaveCall::SELECTOR),
                _ => {}
            }
        }
        if let Ok(preflight) = preflight_evidence_mutator_call(&raw) {
            let selector: [u8; 4] = raw[..4].try_into().unwrap();
            prop_assert_eq!(canonical_preflight_reencode(selector, &preflight), raw);
        }
    }

    #[test]
    fn proptest_preflight_rejects_mutated_offset_and_length_words(
        is_register in any::<bool>(),
        slot_seed in any::<usize>(),
        replacement in teeregistry_word_strategy(),
    ) {
        let mut encoded = if is_register {
            register_call(ValidatorNodeBindingV1::CANONICAL_LEN, 65, 65)
        } else {
            renewal_call(65, 64)
        };
        let head_words = if is_register { 6 } else { 3 };
        let mut candidate_words: Vec<usize> = (0..head_words).collect();
        for head_idx in 0..head_words {
            let off_start = 4 + head_idx * 32;
            let dyn_offset = U256::from_be_slice(&encoded[off_start..off_start + 32]).to::<usize>();
            candidate_words.push(dyn_offset / 32);
        }
        let word_idx = candidate_words[slot_seed % candidate_words.len()];
        let start = 4 + word_idx * 32;
        let canonical_word: [u8; 32] = encoded[start..start + 32].try_into().unwrap();
        encoded[start..start + 32].copy_from_slice(&replacement);

        let result = preflight_evidence_mutator_call(&encoded);
        let is_evidence_len_word = word_idx == head_words;
        // Fixture `evidence` has non-zero prefix bytes followed by zero padding to a 32-byte word,
        // so lengths in `canonical_len..=padded_len` keep `next_offset` and zero tail padding valid.
        let valid_evidence_len = is_evidence_len_word && {
            let canonical_len = U256::from_be_slice(&canonical_word).to::<usize>();
            let padded_len = canonical_len.next_multiple_of(32);
            const UPPER: usize = 32 - core::mem::size_of::<usize>();
            replacement[..UPPER].iter().all(|&b| b == 0) && {
                let mut buf = [0u8; core::mem::size_of::<usize>()];
                buf.copy_from_slice(&replacement[UPPER..]);
                (canonical_len..=padded_len).contains(&usize::from_be_bytes(buf))
            }
        };
        if replacement == canonical_word || valid_evidence_len {
            let preflight = result.expect("canonical layout must pass preflight");
            let selector: [u8; 4] = encoded[..4].try_into().unwrap();
            prop_assert_eq!(canonical_preflight_reencode(selector, &preflight), encoded);
        } else {
            prop_assert!(matches!(result, Err(PrecompileError::RevertBytes(_))));
        }
    }
}
