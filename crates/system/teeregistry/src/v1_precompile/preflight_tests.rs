use super::*;

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

fn assert_revert(encoded: &[u8], expected: &str) {
    let error = preflight_evidence_mutator_call(encoded).err();
    assert!(
        matches!(&error, Some(PrecompileError::Revert(message)) if message == expected),
        "expected revert {expected:?}, got {error:?}"
    );
}

#[test]
fn initial_offsets_are_decoded_before_checking_evidence_offset() {
    for mut encoded in valid_calls() {
        encoded[35] = 0;
        encoded[36] = 1;
        assert_revert(
            &encoded,
            "invalid canonical V1 registration ABI: ABI integer exceeds host usize",
        );
    }
}

#[test]
fn common_offsets_and_signature_lengths_keep_their_exact_errors() {
    for encoded in valid_calls() {
        for (word, label) in [
            (0, "evidence"),
            (1, "node signature"),
            (2, "enclave signature"),
        ] {
            let mut changed = encoded.clone();
            changed[4 + word * 32..4 + (word + 1) * 32].fill(0);
            assert_revert(
                &changed,
                &format!("invalid canonical V1 registration ABI: non-canonical {label} offset"),
            );
        }
    }
    assert_revert(
        &renewal_call(64, 64),
        "node proof-of-possession signature must be 65 bytes",
    );
    assert_revert(
        &renewal_call(65, 63),
        "enclave proof-of-possession signature must be 64 bytes",
    );
}

#[test]
fn validator_offsets_are_decoded_before_checking_binding_offset() {
    let mut encoded = register_call(ValidatorNodeBindingV1::CANONICAL_LEN, 65, 65);
    encoded[100..132].fill(0);
    encoded[132] = 1;
    assert_revert(
        &encoded,
        "invalid canonical V1 registration ABI: ABI integer exceeds host usize",
    );
}

#[test]
fn validator_binding_and_signature_lengths_keep_their_exact_errors() {
    for (binding_len, validator_len, node_len, expected) in [
        (
            ValidatorNodeBindingV1::CANONICAL_LEN - 1,
            65,
            65,
            format!(
                "validator NodeHost binding must be {} bytes",
                ValidatorNodeBindingV1::CANONICAL_LEN
            ),
        ),
        (
            ValidatorNodeBindingV1::CANONICAL_LEN,
            64,
            65,
            "validator NodeHost binding signature must be 65 bytes".into(),
        ),
        (
            ValidatorNodeBindingV1::CANONICAL_LEN,
            65,
            64,
            "NodeHost binding signature must be 65 bytes".into(),
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
        assert_revert(
            &trailing,
            "invalid canonical V1 registration ABI: trailing ABI bytes",
        );
        let mut padding = trailing;
        let evidence_offset = U256::from_be_slice(&encoded[4..36]).to::<usize>();
        padding[4 + evidence_offset + 32 + 3] = 1;
        assert_revert(
            &padding,
            "invalid canonical V1 registration ABI: non-zero dynamic padding",
        );
    }
    assert_revert(
        &[0, 0, 0],
        "invalid canonical V1 registration ABI: missing function selector",
    );
    assert_revert(
        &[0, 0, 0, 0],
        "invalid canonical V1 registration ABI: truncated argument head",
    );
}
