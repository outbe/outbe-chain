use super::*;
use alloy_sol_types::SolError;

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
