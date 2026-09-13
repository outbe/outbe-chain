use super::*;

pub(super) fn b256(last: u8) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[31] = last;
    B256::from(bytes)
}

pub(super) fn marker(height: u64) -> FinalizedMarker {
    FinalizedMarker {
        commitment_scheme_version: 1,
        height,
        block_hash: b256(u8::try_from(height).unwrap()),
        parent_block_hash: b256(u8::try_from(height.saturating_sub(1)).unwrap()),
        parent_root: b256(u8::try_from(height.saturating_add(10)).unwrap()),
        new_root: b256(u8::try_from(height.saturating_add(11)).unwrap()),
    }
}

pub(super) fn identity() -> EnvironmentIdentity {
    EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 99,
        genesis_hash: b256(42),
        commitment_scheme_version: 1,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
    }
}

pub(super) fn sharded_identity(_shard_count: u32) -> EnvironmentIdentity {
    identity()
}

pub(super) fn sharded_genesis(shard_count: u32) -> FinalizedMarker {
    let identity = sharded_identity(shard_count);
    FinalizedMarker {
        commitment_scheme_version: identity.commitment_scheme_version,
        height: 0,
        block_hash: identity.genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    }
}
