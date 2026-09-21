use super::*;

pub(super) fn test_chain_spec() -> std::sync::Arc<ChainSpec<OutbeHeader>> {
    let mut spec = MAINNET.as_ref().clone();
    spec.chain = GRAMINE_DIRECT_DEV_CHAIN_ID.into();
    spec.genesis.config.chain_id = GRAMINE_DIRECT_DEV_CHAIN_ID;
    spec.map_header(OutbeHeader::new).into()
}

pub(super) fn test_parent() -> SealedHeader<OutbeHeader> {
    SealedHeader::seal_slow(OutbeHeader::new(Header::default()))
}

pub(super) fn test_parent_with_millis_part(
    timestamp_millis_part: u64,
) -> SealedHeader<OutbeHeader> {
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        timestamp_millis_part,
        ..Default::default()
    })
    .expect("encode artifacts for test parent");
    let inner = Header {
        extra_data,
        ..Default::default()
    };
    SealedHeader::seal_slow(OutbeHeader::new(inner))
}

pub(super) fn test_summary() -> ExecutionSummaryArtifact {
    ExecutionSummaryArtifact {
        validator_fee_sum: U256::from(33u64),
    }
}

pub(super) fn next_block_attrs(extra_data: Bytes) -> OutbeNextBlockEnvAttributes {
    OutbeNextBlockEnvAttributes {
        inner: NextBlockEnvAttributes {
            timestamp: 1,
            suggested_fee_recipient: Address::ZERO,
            prev_randao: B256::ZERO,
            gas_limit: 30_000_000,
            parent_beacon_block_root: None,
            withdrawals: None,
            extra_data,
            slot_number: None,
        },
        timestamp_millis_part: 0,
        parent_consensus_metadata: None,
        proposer_evm_address: None,
        execute_outbe_block_hooks: true,
        prebuilt_phase1_tx: None,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    }
}
