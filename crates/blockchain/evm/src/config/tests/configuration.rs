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

    for plain in [&b""[..], b"x", b"OAR", b"reth/vtest/macos"] {
        let ctx = config
            .context_for_next_block(&parent, next_block_attrs(Bytes::copy_from_slice(plain)))
            .expect("context construction must succeed");
        assert!(ctx.inner.extra_data.is_empty());
        assert!(ctx.system_layout_error.is_none());
    }
}

#[test]
fn context_for_next_block_uses_outbe_parent_hash() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    use alloy_primitives::{address, b256, hex, keccak256, B64};

    // Geth v1.16.5 TestBlockEncoding pins this header and its hash independently.
    // https://github.com/ethereum/go-ethereum/blob/v1.16.5/core/types/block_test.go
    let parent = SealedHeader::seal_slow(OutbeHeader::new(Header {
        parent_hash: b256!("83cafc574e1f51ba9dc0568fc617a08ea2429fb384059c972f13b19fa1c8dd55"),
        ommers_hash: b256!("1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347"),
        beneficiary: address!("8888f1f195afa192cfee860698584c030f4c9db1"),
        state_root: b256!("ef1552a40b7165c3cd773806b9e0c165b75356e0314bf0706f279c729f51e017"),
        transactions_root: b256!(
            "5fe50b260da6308036625b850b5d6ced6d0a9f814c0688bc91ffb7b7a3a54b67"
        ),
        receipts_root: b256!("bc37d79753ad738a6dac4921e57392f145d8887476de3f783dfa7edae9283e52"),
        logs_bloom: Default::default(),
        difficulty: U256::from(131_072),
        number: 1,
        gas_limit: 3_141_592,
        gas_used: 21_000,
        timestamp: 1_426_516_743,
        extra_data: Bytes::new(),
        mix_hash: b256!("bd4472abb6659ebe3ee06ee4d7b72a00a9f4d001caca51342001075469aff498"),
        nonce: B64::from(0xa13a5a8c8f2bb1c4_u64.to_be_bytes()),
        ..Default::default()
    }));
    let expected_rlp = hex!(
        "f901f9a083cafc574e1f51ba9dc0568fc617a08ea2429fb384059c972f13b19fa1c8dd55a01dcc4de8dec75d7aab85b5"
        "67b6ccd41ad312451b948a7413f0a142fd40d49347948888f1f195afa192cfee860698584c030f4c9db1a0ef1552a40b"
        "7165c3cd773806b9e0c165b75356e0314bf0706f279c729f51e017a05fe50b260da6308036625b850b5d6ced6d0a9f81"
        "4c0688bc91ffb7b7a3a54b67a0bc37d79753ad738a6dac4921e57392f145d8887476de3f783dfa7edae9283e52b90100"
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"
        "000000000000000000000000000000008302000001832fefd8825208845506eb0780a0bd4472abb6659ebe3ee06ee4d7"
        "b72a00a9f4d001caca51342001075469aff49888a13a5a8c8f2bb1c4"
    );
    let expected_hash = b256!("0a5843ac1cb04865017cb35a57b50b07084e5fcee39b5acadade33149f4fff9e");
    assert_eq!(keccak256(expected_rlp), expected_hash);
    assert_eq!(parent.hash(), expected_hash);

    let ctx = config
        .context_for_next_block(&parent, next_block_attrs(Bytes::new()))
        .expect("context construction must succeed");

    assert_eq!(ctx.inner.parent_hash, parent.hash());
    assert_eq!(ctx.inner.parent_hash, expected_hash);
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
    assert!(ctx.system_layout_error.is_none());
}

#[test]
fn context_for_next_block_rejects_malformed_or_unsupported_oart_before_execution() {
    use crate::errors::SystemTxExpectationError;
    use alloy_evm::block::BlockExecutor as _;
    use reth_evm::execute::BlockBuilder as _;
    use revm::database::State;

    let config = OutbeEvmConfig::new(test_chain_spec());
    let parent = test_parent();
    for extra_data in [
        &b"OART"[..],
        b"OART\x01\x00\xff",
        b"OART\x01\x00\x01\x02\x00\x10\xaa",
        b"OART\x01\x00\x01\xff\x00\x00",
        b"OART\x7f\x00\x00",
        b"OART\x05\x00\x00",
    ] {
        let mut attrs = next_block_attrs(Bytes::copy_from_slice(extra_data));
        attrs.inner.suggested_fee_recipient = outbe_primitives::addresses::REWARDS_ADDRESS;
        let ctx = config
            .context_for_next_block(&parent, attrs.clone())
            .unwrap();
        assert!(
            matches!(
                ctx.system_layout_error,
                Some(SystemTxExpectationError::DecodeBlockArtifacts(_))
            ),
            "malformed or unsupported OART must retain its decode error: {extra_data:?}"
        );

        let mut state = State::builder().with_bundle_update().build();
        let mut builder = config
            .builder_for_next_block(&mut state, &parent, attrs)
            .unwrap();
        let error = builder
            .apply_pre_execution_changes()
            .expect_err("invalid OART must reject the block");
        assert!(
            error
                .to_string()
                .contains("invalid system tx layout: decode Outbe block artifacts"),
            "{error}"
        );
        assert!(builder.executor().receipts().is_empty());
        drop(builder);
        assert!(state.bundle_state.state.is_empty());
        assert!(state
            .transition_state
            .as_ref()
            .unwrap()
            .transitions
            .is_empty());
    }
}

/// `sanitize_next_block_extra_data` must PRESERVE a non-empty
/// `late_finalize_credits` artifact (while it resets `execution_summary` and
/// `timestamp_millis_part`, which the payload builder recomputes). Otherwise,
/// the sanitizer would silently drop the proposer-packed late credits before
/// sealing.
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
        radicle_publication_enabled: false,
        radicle_publication: None,
        execution_summary: Some(ExecutionSummaryArtifact {
            validator_fee_sum: U256::from(123u64),
        }),
        consensus_header_artifact: None,
        timestamp_millis_part: 777,
        late_finalize_credits: Some(credits.clone()),
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
    assert!(ctx.system_layout_error.is_none());
}

#[test]
fn published_eip155_transaction_recovers_fixed_sender_and_chain() {
    use alloy_consensus::Transaction as _;
    use alloy_eips::eip2718::{Decodable2718, Encodable2718};
    use alloy_primitives::{address, hex};
    use reth_ethereum::TransactionSigned;
    use reth_primitives_traits::SignedTransaction as _;

    // Geth v1.16.5 TestEIP155SigningVitalik, first vector, chain ID 1.
    // https://github.com/ethereum/go-ethereum/blob/v1.16.5/core/types/transaction_signing_test.go
    let raw = hex!(
        "f864808504a817c800825208943535353535353535353535353535353535353535808025"
        "a0044852b2a670ade5407e78fb2863c51de9fcb96542a07186fe3aeda6bb8a116d"
        "a0044852b2a670ade5407e78fb2863c51de9fcb96542a07186fe3aeda6bb8a116d"
    );
    let mut input = raw.as_slice();
    let tx = TransactionSigned::decode_2718(&mut input).expect("decode published transaction");
    assert!(input.is_empty());
    assert_eq!(tx.chain_id(), Some(1));
    assert_eq!(
        tx.try_recover().expect("recover published sender"),
        address!("f0f6f18bca1b28cd68e4357452947e021241e9ce")
    );
    assert_eq!(tx.encoded_2718(), raw);
}
