use super::*;

#[test]
fn new_with_bridge_installs_cache_summary_provider() {
    let bridge = ConsensusExecutionBridge::new();
    let summary = test_summary();
    let block_hash = B256::repeat_byte(0x52);
    bridge.record_execution_summary(8, block_hash, summary, 456);
    let config = OutbeEvmConfig::new_with_bridge(test_chain_spec(), bridge);

    let provider = config
        .accounted_parent_artifact_provider
        .as_ref()
        .expect("bridge config must install summary provider");
    let resolved = provider
        .execution_summary_by_hash(8, block_hash)
        .expect("cache provider must not fail")
        .expect("bridge cache must resolve summary");

    assert_eq!(resolved.summary, summary);
    assert_eq!(resolved.timestamp, 456);
    assert_eq!(resolved.state_root, None);
}

#[test]
fn config_fixture_uses_the_gramine_direct_dev_chain_identity() {
    let chain_spec = test_chain_spec();
    assert_eq!(chain_spec.chain().id(), GRAMINE_DIRECT_DEV_CHAIN_ID);
    assert_eq!(
        chain_spec.genesis.config.chain_id,
        GRAMINE_DIRECT_DEV_CHAIN_ID
    );
}

#[test]
fn evm_config_binds_factory_to_canonical_chain_spec_genesis() {
    let chain_spec = test_chain_spec();
    let expected = chain_spec.genesis_hash();
    let config = OutbeEvmConfig::new(chain_spec);

    assert_eq!(
        config.inner.executor_factory.evm_factory().genesis_hash(),
        expected
    );
}

#[test]
fn pending_env_disables_outbe_hooks_and_uses_rewards_beneficiary() {
    let parent = test_parent_with_millis_part(321);

    let attrs = OutbeNextBlockEnvAttributes::build_pending_env(&parent, None);

    assert_eq!(
        attrs.inner.suggested_fee_recipient,
        outbe_primitives::addresses::REWARDS_ADDRESS
    );
    assert_eq!(attrs.timestamp_millis_part, 321);
    assert!(attrs.parent_consensus_metadata.is_none());
    assert!(attrs.proposer_evm_address.is_none());
    assert!(!attrs.execute_outbe_block_hooks);
}

#[test]
fn context_for_next_block_strips_plain_builder_extra_data() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let parent = test_parent();

    let ctx = config
        .context_for_next_block(
            &parent,
            next_block_attrs(Bytes::from_static(b"reth/vtest/macos")),
        )
        .expect("context construction must succeed");

    assert!(
        ctx.inner.extra_data.is_empty(),
        "plain reth builder extra_data must not enter Outbe header artifact path"
    );
}

#[test]
fn context_for_next_block_uses_outbe_parent_hash() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let parent = test_parent_with_millis_part(7);
    // Post-refactor (sub-second timestamp moved into `extra_data`)
    // the wrapper hash and the inner Ethereum hash are identical
    // by design - that is the Ethereum-spec compatibility this
    // refactor guarantees. The test still verifies that
    // `context_for_next_block` propagates the sealed parent hash
    // unchanged.
    let inner_parent_hash = parent.header().inner.hash_slow();

    let ctx = config
        .context_for_next_block(&parent, next_block_attrs(Bytes::new()))
        .expect("context construction must succeed");

    assert_eq!(ctx.inner.parent_hash, parent.hash());
    assert_eq!(ctx.inner.parent_hash, inner_parent_hash);
}

#[test]
fn context_for_next_block_preserves_valid_consensus_header_artifact() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let parent = test_parent();
    let artifact = encode_consensus_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(
        DkgBoundaryArtifact {
            epoch: 7,
            dkg_cycle: 1,
            freeze_height: 100,
            planned_activation_height: 200,
            target_set_hash: B256::repeat_byte(0x21),
            vrf_material_version: 1,
            vrf_group_public_key: B256::repeat_byte(0x22),
            vrf_group_public_key_bytes: Bytes::from_static(&[0x22u8; 96]),
            committee_set_hash: B256::repeat_byte(0x23),
            is_validator_set_change: true,
            outcome: Bytes::from_static(b"outcome"),
            is_full_dkg: true,
            tee_recipient_pubkeys: Vec::new(),
            tee_expired_target_exclusions: Vec::new(),
            tee_expired_target_exclusions_hash: B256::ZERO,
            reshare: ReshareResult {
                new_active_set: vec![Address::repeat_byte(0x11)],
                active_set_hash: B256::repeat_byte(0x21),
            },
        },
    ))
    .expect("artifact encoding must succeed");

    let ctx = config
        .context_for_next_block(&parent, next_block_attrs(artifact.clone()))
        .expect("context construction must succeed");

    assert_eq!(ctx.inner.extra_data, artifact);
}

#[test]
fn context_for_next_block_drops_legacy_finalization_header_tag() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let parent = test_parent();
    let extra_data = Bytes::from_static(b"OART\x05\x01\x04\x00\x00");

    let ctx = config
        .context_for_next_block(&parent, next_block_attrs(extra_data))
        .expect("context construction must succeed");

    assert!(
        ctx.inner.extra_data.is_empty(),
        "legacy finalized-parent header tag must not survive into next-block context"
    );
}

/// `sanitize_next_block_extra_data` must PRESERVE a non-empty
/// `late_finalize_credits` artifact (while resetting `execution_summary` and
/// `timestamp_millis_part`, which the payload builder recomputes) - otherwise
/// the proposer-packed late credits would be silently dropped before sealing.
#[test]
fn sanitizer_preserves_late_credits() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let parent = test_parent();
    let credits = LateFinalizeCreditsArtifact {
        batches: vec![PerBlockCredit {
            fb_number: 9,
            fb_hash: B256::repeat_byte(0x4b),
            epoch: 2,
            view: 11,
            parent_view: 10,
            committee_set_hash: B256::repeat_byte(0xEF),
            signer_bitmap: vec![0x05],
            aggregate_signature: [7u8; 96],
        }],
    };
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(123u64),
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 777,
        late_finalize_credits: Some(credits.clone()),
        compressed_entities_root: None,
    })
    .expect("encode");

    let ctx = config
        .context_for_next_block(&parent, next_block_attrs(extra_data))
        .expect("context construction must succeed");

    let decoded =
        decode_outbe_block_artifacts(ctx.inner.extra_data.as_ref()).expect("decode sanitized");
    assert_eq!(
        decoded.late_finalize_credits,
        Some(credits),
        "late_finalize_credits must survive the next-block sanitizer"
    );
    assert!(
        decoded.execution_summary.is_none(),
        "execution_summary is reset by the sanitizer (payload builder recomputes it)"
    );
    assert_eq!(
        decoded.timestamp_millis_part, 0,
        "timestamp_millis_part is reset by the sanitizer"
    );
}
