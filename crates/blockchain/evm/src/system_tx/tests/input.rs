use super::*;
#[test]
fn assembler_uses_only_active_builders_and_preserves_payloads() {
    use outbe_primitives::{
        reshare_artifact::{ConsensusHeaderArtifact, OutbeBlockArtifacts},
        system_tx_context::{SystemTxBuildContext, SystemTxPayloadBuildError},
    };
    let parent = sample_metadata();
    let bootstrap_input = bootstrap();
    let tee =
        outbe_primitives::tee_bootstrap_v2::codec::decode_canonical(bootstrap_input.payload())
            .unwrap();
    let boundary_input = boundary();
    let boundary_artifact =
        outbe_primitives::reshare_artifact::decode_boundary_artifact(boundary_input.payload())
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

#[test]
fn assembler_discards_begin_block_builder_bytes() {
    use {
        crate::system_tx::{begin_step, ProtocolStep},
        outbe_primitives::{
            block::ProtocolModule,
            reshare_artifact::OutbeBlockArtifacts,
            system_tx_context::{SystemTxBuildContext, SystemTxPayloadBuildError},
        },
    };
    struct NonEmpty;
    impl ProtocolModule for NonEmpty {
        const NAME: &'static str = "non_empty";
        fn build_payload(_: &SystemTxBuildContext<'_>) -> Result<Bytes, SystemTxPayloadBuildError> {
            Ok(Bytes::from_static(b"discarded"))
        }
    }
    const BEGIN: &[ProtocolStep] = &[begin_step::<NonEmpty>()];
    let order = BlockOrder {
        begin_block: BEGIN,
        ..TEST_BLOCK_ORDER
    };
    let artifacts = OutbeBlockArtifacts::default();
    let metadata = sample_metadata();
    let ctx = SystemTxBuildContext {
        regime: BlockHeightRegime::SteadyState,
        block_number: 2,
        parent_hash: metadata.finalized_block_hash,
        artifacts: &artifacts,
        parent_consensus_metadata: Some(&metadata),
        pending_tee_bootstrap: None,
        expected_tee_policy: None,
    };
    assert_eq!(
        assemble_prefix_system_txs(&order, &ctx).unwrap(),
        vec![
            accounting(),
            late_credits(),
            fixtures::consensus(4, Bytes::new()),
            SystemTx::BeginBlock { step_index: 0 }
        ]
    );
}
