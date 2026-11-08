use super::*;

#[test]
fn input_roundtrips_every_system_tx_variant() {
    assert_eq!(inputs().len(), ALL_KINDS.len());
    for (input, kind) in inputs().into_iter().zip(ALL_KINDS) {
        let encoded = input.encode().expect("input encodes");
        assert_eq!(&encoded[..4], &kind.selector());
        assert_eq!(encoded[4], SYSTEM_TX_INPUT_VERSION);
        let decoded = SystemTx::decode(&encoded).expect("input decodes");
        assert_eq!(decoded, input);
        assert_eq!(decoded.kind(), kind);
        assert_eq!(decoded.selector(), kind.selector());
        assert_eq!(decoded.name(), kind.name());
        assert_eq!(SystemTxKind::from_selector(kind.selector()), Some(kind));
    }
}

#[test]
fn decode_preserves_selector_version_and_body_validation_precedence() {
    for len in 0..5 {
        assert_eq!(
            SystemTx::decode(&b"OSC2\x02"[..len]),
            Err(SystemTxError::InputTooShort { len })
        );
    }
    for unknown in [
        *b"NOPE", *b"OSO2", *b"OSH2", *b"OSCE", *b"OSCP", *b"OSF1", *b"OSC1", *b"OSB1", *b"OSO1",
    ] {
        let mut bytes = Vec::with_capacity(5);
        bytes.extend_from_slice(&unknown);
        bytes.push(SYSTEM_TX_INPUT_VERSION);
        assert_eq!(
            SystemTx::decode(&bytes),
            Err(SystemTxError::UnknownSelector(unknown))
        );
    }
    for input in inputs() {
        let mut unsupported = input.encode().unwrap().to_vec();
        unsupported[4] = 1;
        assert_eq!(
            SystemTx::decode(&unsupported),
            Err(SystemTxError::UnsupportedVersion(1))
        );
    }
    for kind in ALL_KINDS {
        if let Some(empty_tx) = kind.into_empty_system_tx() {
            let mut nonempty = empty_tx.encode().unwrap().to_vec();
            nonempty.push(0);
            assert_eq!(
                SystemTx::decode(&nonempty),
                Err(SystemTxError::UnexpectedBody {
                    kind: empty_tx.name(),
                    len: 1
                })
            );
        }
    }
    assert_eq!(
        SystemTx::decode(b"OSB2\x02"),
        Err(SystemTxError::MissingBoundaryOutcomeBody)
    );
    assert_eq!(SystemTx::decode(b"OSL2\x02"), Ok(late_credits()));
    for malformed in [b"OSA3\x02".as_slice(), b"OST3\x02", b"OSP2\x02"] {
        assert!(matches!(
            SystemTx::decode(malformed),
            Err(SystemTxError::Codec(_))
        ));
    }
}
