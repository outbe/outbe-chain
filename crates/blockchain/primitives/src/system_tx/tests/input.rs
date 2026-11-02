use super::*;

#[test]
fn input_roundtrips_every_system_tx_variant() {
    for (input, (selector, name)) in inputs().into_iter().zip([
        (*b"OSA3", "CertifiedParentAccounting"),
        (*b"OSL2", "LateFinalizeCredits"),
        (*b"OSC2", "CycleTick"),
        (*b"OSB2", "BoundaryOutcome"),
        (*b"OST3", "TeeBootstrap"),
        (*b"OSO2", "OracleSlashWindow"),
        (*b"OSH2", "HookEvents"),
        (*b"OSP2", "RadiclePublication"),
    ]) {
        let encoded = input.encode().expect("input encodes");
        assert_eq!(&encoded[..4], &selector);
        assert_eq!(encoded[4], 2);
        let decoded = SystemTx::decode(&encoded).expect("input decodes");
        assert_eq!(decoded, input);
        assert_eq!(decoded.selector(), selector);
        assert_eq!(decoded.name(), name);
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
    assert_eq!(
        SystemTx::decode(b"NOPE\x01"),
        Err(SystemTxError::UnknownSelector(*b"NOPE"))
    );
    for input in inputs() {
        let mut unsupported = input.encode().unwrap().to_vec();
        unsupported[4] = 1;
        assert_eq!(
            SystemTx::decode(&unsupported),
            Err(SystemTxError::UnsupportedVersion(1))
        );
    }
    for input in [
        SystemTx::CycleTick,
        SystemTx::OracleSlashWindow,
        SystemTx::HookEvents,
    ] {
        let mut nonempty = input.encode().unwrap().to_vec();
        nonempty.push(0);
        assert_eq!(
            SystemTx::decode(&nonempty),
            Err(SystemTxError::UnexpectedBody {
                kind: input.name(),
                len: 1
            })
        );
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
