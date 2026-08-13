use outbe_chain_constants::{
    GenesisProtocolParametersV1, CHAIN_CONSTANTS_ADDRESS,
    DEFAULT_NOD_MATERIALIZATION_BATCH_SUBTREE_HEIGHT,
    DEFAULT_NOD_MATERIALIZATION_MAX_ATTEMPTS_PER_BLOCK,
    DEFAULT_NOD_MATERIALIZATION_RETRY_INTERVAL_BLOCKS,
};
use serde_json::json;

#[test]
fn missing_materialization_profile_resolves_to_canonical_defaults() {
    let resolved = GenesisProtocolParametersV1::resolve(None).expect("default protocol parameters");

    assert_eq!(
        resolved.nod_materialization.batch_subtree_height,
        DEFAULT_NOD_MATERIALIZATION_BATCH_SUBTREE_HEIGHT
    );
    assert_eq!(resolved.nod_materialization.batch_capacity().unwrap(), 8);
    assert_eq!(
        resolved.nod_materialization.retry_interval_blocks,
        DEFAULT_NOD_MATERIALIZATION_RETRY_INTERVAL_BLOCKS
    );
    assert_eq!(
        resolved.nod_materialization.max_attempts_per_block,
        DEFAULT_NOD_MATERIALIZATION_MAX_ATTEMPTS_PER_BLOCK
    );
}

#[test]
fn explicit_materialization_profile_is_genesis_bound_and_hash_covered() {
    let defaults = GenesisProtocolParametersV1::default();
    let resolved = GenesisProtocolParametersV1::resolve(Some(&json!({
        "nodMaterialization": {
            "batchSubtreeHeight": 2,
            "retryIntervalBlocks": 12,
            "maxAttemptsPerBlock": 2
        }
    })))
    .expect("explicit materialization profile");

    assert_eq!(resolved.nod_materialization.batch_capacity().unwrap(), 4);
    assert_eq!(resolved.nod_materialization.retry_interval_blocks, 12);
    assert_eq!(resolved.nod_materialization.max_attempts_per_block, 2);
    assert_ne!(resolved.parameters_hash(), defaults.parameters_hash());

    let storage = resolved
        .genesis_storage_words()
        .into_iter()
        .map(|(slot, value)| {
            (
                format!("{slot:#066x}"),
                serde_json::Value::String(format!("{value:#066x}")),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let mut alloc = serde_json::Map::new();
    alloc.insert(
        format!("{CHAIN_CONSTANTS_ADDRESS:#x}"),
        json!({ "code": "0xef", "storage": storage }),
    );
    let genesis = json!({ "alloc": alloc });
    assert_eq!(
        GenesisProtocolParametersV1::from_materialized_genesis(&genesis).unwrap(),
        resolved
    );
}

#[test]
fn partial_materialization_profile_uses_canonical_defaults_for_missing_fields() {
    let resolved = GenesisProtocolParametersV1::resolve(Some(&json!({
        "nodMaterialization": { "retryIntervalBlocks": 12 }
    })))
    .expect("partial materialization profile");

    assert_eq!(
        resolved.nod_materialization.batch_subtree_height,
        DEFAULT_NOD_MATERIALIZATION_BATCH_SUBTREE_HEIGHT
    );
    assert_eq!(resolved.nod_materialization.retry_interval_blocks, 12);
    assert_eq!(
        resolved.nod_materialization.max_attempts_per_block,
        DEFAULT_NOD_MATERIALIZATION_MAX_ATTEMPTS_PER_BLOCK
    );
}

#[test]
fn invalid_materialization_profiles_fail_instead_of_falling_back() {
    for profile in [
        json!({ "batchSubtreeHeight": 9 }),
        json!({ "retryIntervalBlocks": 0 }),
        json!({ "maxAttemptsPerBlock": 0 }),
    ] {
        assert!(GenesisProtocolParametersV1::resolve(Some(&json!({
            "nodMaterialization": profile
        })))
        .is_err());
    }
}
