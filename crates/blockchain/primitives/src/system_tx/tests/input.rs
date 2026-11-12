use super::*;

#[test]
fn input_roundtrips_every_system_tx_variant() {
    for input in inputs() {
        let encoded = input.encode().expect("input encodes");
        assert_eq!(&encoded[..4], &input.selector());
        assert_eq!(encoded[4], 3);
        assert_eq!(encoded[5], input.step_index());
        assert_eq!(SystemTx::decode(&encoded).unwrap(), input);
    }
}

#[test]
fn decode_preserves_selector_version_and_body_validation_precedence() {
    for len in 0..6 {
        assert_eq!(
            SystemTx::decode(&b"OSBB\x03\x00"[..len]),
            Err(SystemTxError::InputTooShort { len })
        );
    }
    for unknown in [
        *b"NOPE", *b"OSA3", *b"OSL2", *b"OSB2", *b"OST3", *b"OSP2", *b"OSC2", *b"OSVT",
    ] {
        let mut bytes = unknown.to_vec();
        bytes.extend([3, 0]);
        assert_eq!(
            SystemTx::decode(&bytes),
            Err(SystemTxError::UnknownSelector(unknown))
        );
    }
    for input in inputs() {
        let mut unsupported = input.encode().unwrap().to_vec();
        unsupported[4] = 2;
        assert_eq!(
            SystemTx::decode(&unsupported),
            Err(SystemTxError::UnsupportedVersion(2))
        );
    }
    for input in begin_stage_inputs().into_iter().chain(end_stage_inputs()) {
        let mut nonempty = input.encode().unwrap().to_vec();
        nonempty.push(0);
        assert_eq!(
            SystemTx::decode(&nonempty),
            Err(SystemTxError::NonEmptyStagePayload {
                stage: input.stage(),
                len: 1
            })
        );
    }
}

#[test]
fn stage_framing_accepts_consensus_payload_and_empty_lifecycle_steps() {
    for bytes in [
        b"OSCN\x03\x04payload".as_slice(),
        b"OSBB\x03\x00",
        b"OSEB\x03\x04",
    ] {
        let decoded = SystemTx::decode(bytes).expect("v3 six-byte stage header");
        assert_eq!(decoded.encode().unwrap().as_ref(), bytes);
    }
}

#[test]
fn end_block_ordinals_are_checked_without_wrapping() {
    for index in 0..=255 {
        let expected = match index {
            0..=7 => Some(24 + index),
            _ => None,
        };
        assert_eq!(end_block_nonce_ordinal(index), expected);
        assert_eq!(
            SystemTx::EndBlock { step_index: index }.nonce_ordinal(),
            expected
        );
    }
    assert_eq!(SystemTx::BeginBlock { step_index: 0 }.nonce_ordinal(), None);
    assert_eq!(fixtures::consensus(0, Bytes::new()).nonce_ordinal(), None);
}
