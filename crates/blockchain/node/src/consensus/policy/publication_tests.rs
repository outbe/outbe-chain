use super::*;
use alloy_consensus::Header;
use alloy_primitives::{B256, U256};
use outbe_primitives::{
    addresses::RADICLE_REGISTRY_ADDRESS,
    radicle_genesis::{
        GovernmentsDescriptor, GENESIS_GOVERNMENTS_FIELD, GOVERNMENTS_DESCRIPTOR_HASH_SLOT,
    },
    reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts},
};
use reth_chainspec::{ChainSpec, MAINNET};

fn policy(enabled: bool) -> OutbeConsensusPolicy<ChainSpec<OutbeHeader>> {
    let mut spec = MAINNET.as_ref().clone().map_header(OutbeHeader::new);
    if enabled {
        let descriptor = GovernmentsDescriptor {
            chain_id: spec.chain().id(),
            genesis_timestamp: spec.genesis.timestamp,
            description: "Network governance".into(),
            initial_tree_oid: [1; 20].into(),
            initial_content_manifest_sha256: B256::repeat_byte(2),
        };
        spec.genesis.config.extra_fields.insert(
            GENESIS_GOVERNMENTS_FIELD.into(),
            serde_json::Value::String(format!("0x{}", hex::encode(descriptor.to_bytes().unwrap()))),
        );
        spec.genesis
            .alloc
            .entry(RADICLE_REGISTRY_ADDRESS)
            .or_default()
            .storage
            .get_or_insert_default()
            .insert(
                U256::from(GOVERNMENTS_DESCRIPTOR_HASH_SLOT)
                    .to_be_bytes::<32>()
                    .into(),
                descriptor.hash().unwrap(),
            );
    }
    OutbeConsensusPolicy {
        chain_spec: Arc::new(spec),
        skip_gas_limit_ramp_check: true,
        ocomp_lifecycle_activation: OcompLifecycleActivation::Disabled,
    }
}

fn header(enabled: bool) -> OutbeHeader {
    OutbeHeader::new(Header {
        number: 1,
        extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            radicle_publication_enabled: enabled,
            timestamp_millis_part: 1,
            ..Default::default()
        })
        .unwrap(),
        ..Default::default()
    })
}

#[test]
fn header_mode_is_derived_from_descriptor_committed_in_genesis_state() {
    assert!(policy(false).validate_header(&header(false)).is_ok());
    assert!(policy(false).validate_header(&header(true)).is_err());
    assert!(policy(true).validate_header(&header(true)).is_ok());
    assert!(policy(true).validate_header(&header(false)).is_err());
    let mut legacy = header(false);
    legacy.inner.extra_data = b"opaque legacy header bytes".to_vec().into();
    assert!(policy(false).validate_header(&legacy).is_ok());
}

#[test]
fn unseeded_missing_or_mismatched_genesis_descriptor_does_not_enable_publication() {
    let mut unseeded = policy(true);
    Arc::make_mut(&mut unseeded.chain_spec)
        .genesis
        .alloc
        .remove(&RADICLE_REGISTRY_ADDRESS);
    assert!(unseeded.validate_header(&header(true)).is_err());
    let mut missing = policy(true);
    Arc::make_mut(&mut missing.chain_spec)
        .genesis
        .config
        .extra_fields
        .remove(GENESIS_GOVERNMENTS_FIELD);
    assert!(missing.validate_header(&header(true)).is_err());
    let mut mismatch = policy(true);
    let value = Arc::make_mut(&mut mismatch.chain_spec)
        .genesis
        .config
        .extra_fields
        .get_mut(GENESIS_GOVERNMENTS_FIELD)
        .unwrap();
    let mut raw = hex::decode(value.as_str().unwrap().strip_prefix("0x").unwrap()).unwrap();
    raw[43] ^= 1;
    *value = serde_json::Value::String(format!("0x{}", hex::encode(raw)));
    assert!(mismatch.validate_header(&header(true)).is_err());
}
