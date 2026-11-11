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

#[test]
fn assembler_uses_only_active_builders_and_preserves_payloads() {
    use crate::{
        reshare_artifact::{ConsensusHeaderArtifact, OutbeBlockArtifacts},
        system_tx_context::{SystemTxBuildContext, SystemTxPayloadBuildError},
    };
    let parent = sample_metadata();
    let bootstrap_input = bootstrap();
    let tee = crate::tee_bootstrap_v2::codec::decode_canonical(bootstrap_input.payload()).unwrap();
    let boundary_input = boundary();
    let boundary_artifact =
        crate::reshare_artifact::decode_boundary_artifact(boundary_input.payload())
            .unwrap()
            .unwrap();
    let artifacts = OutbeBlockArtifacts {
        consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
            boundary_artifact,
        )),
        ..Default::default()
    };
    let mut ctx = SystemTxBuildContext {
        regime: BlockHeightRegime::Genesis,
        block_number: 0,
        parent_hash: parent.finalized_block_hash,
        artifacts: &artifacts,
        parent_consensus_metadata: None,
        pending_tee_bootstrap: None,
        expected_tee_policy: None,
    };
    assert!(assemble_prefix_system_txs(&TEST_BLOCK_ORDER, &ctx)
        .unwrap()
        .is_empty());
    ctx.regime = BlockHeightRegime::GenesisBootstrap;
    ctx.block_number = 1;
    assert!(matches!(
        assemble_prefix_system_txs(&TEST_BLOCK_ORDER, &ctx),
        Err(SystemTxPayloadBuildError::MissingBlock1TeeBootstrapPayload)
    ));
    ctx.pending_tee_bootstrap = Some(&tee);
    let prefix = assemble_prefix_system_txs(&TEST_BLOCK_ORDER, &ctx).unwrap();
    assert_eq!(prefix.len(), 12);
    assert_eq!(prefix[0], boundary_input);
    assert_eq!(prefix[1], bootstrap_input);
    assert_eq!(prefix[2], fixtures::consensus(4, Bytes::new()));
    assert_eq!(&prefix[3..], &begin_stage_inputs());
    ctx.regime = BlockHeightRegime::SteadyState;
    ctx.block_number = 2;
    ctx.pending_tee_bootstrap = None;
    assert!(matches!(
        assemble_prefix_system_txs(&TEST_BLOCK_ORDER, &ctx),
        Err(SystemTxPayloadBuildError::MissingParentConsensusMetadata { block_number: 2 })
    ));
    ctx.parent_consensus_metadata = Some(&parent);
    let prefix = assemble_prefix_system_txs(&TEST_BLOCK_ORDER, &ctx).unwrap();
    assert_eq!(prefix[0], accounting());
    assert_eq!(prefix[1], late_credits());
    assert_eq!(prefix[2], boundary_input);
    assert_eq!(prefix[3], fixtures::consensus(4, Bytes::new()));
    ctx.parent_hash = B256::repeat_byte(99);
    assert!(matches!(
        assemble_prefix_system_txs(&TEST_BLOCK_ORDER, &ctx),
        Err(SystemTxPayloadBuildError::CertifiedParentAccountingHashMismatch { .. })
    ));
}
