use super::*;

#[test]
fn radicle_channel_is_frozen_before_network_start() {
    assert_eq!(radicle_channel_config(), (8, 32));
}

#[test]
fn testnet_clock_offset_is_rejected_for_unregistered_networks() {
    let unknown_production_chain = 1_000_000_001;
    let error = validate_testnet_only_flags(false, Some(1), unknown_production_chain)
        .unwrap_err()
        .to_string();
    assert!(error.contains("--testnet.unix-time-offset-secs"));
}

#[test]
fn testnet_clock_offset_is_allowed_only_on_explicit_test_networks() {
    for chain_id in [
        outbe_primitives::chain::DEVNET_CHAIN_ID,
        outbe_primitives::chain::TESTNET_CHAIN_ID,
    ] {
        validate_testnet_only_flags(false, Some(-60), chain_id).unwrap();
    }
}

#[test]
fn every_testnet_only_flag_is_rejected_for_unregistered_networks() {
    let chain_id = 1_000_000_001;
    assert!(validate_testnet_only_flags(true, None, chain_id).is_err());
    assert!(validate_testnet_only_flags(false, Some(0), chain_id).is_err());
}

#[test]
fn every_testnet_only_flag_is_rejected_for_mainnet() {
    let chain_id = outbe_primitives::chain::MAINNET_CHAIN_ID;
    assert!(validate_testnet_only_flags(true, None, chain_id).is_err());
    assert!(validate_testnet_only_flags(false, Some(0), chain_id).is_err());
}

#[test]
fn test_build_peer_map_from_bootnodes() {
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();
    let pk_bytes = commonware_codec::Encode::encode(&pk);

    let addr: std::net::SocketAddr = "127.0.0.1:30400".parse().unwrap();
    let mut bootnode_map = std::collections::BTreeMap::new();
    bootnode_map.insert(pk_bytes.to_vec(), addr);

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk.clone()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing], // no static p2p_address
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 1);
}

#[test]
fn test_parse_consensus_peers_rejects_invalid_entries() {
    let err = parse_consensus_peers(&["not-a-peer".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("expected <hex_bls_pubkey>@<host:port>"));

    let err = parse_consensus_peers(&["zz@127.0.0.1:30400".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("public key is not hex"));

    let err = parse_consensus_peers(&["aa@not-a-socket".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid socket address"));
}

#[test]
fn test_require_genesis_hash_rejects_missing_hash() {
    let err = require_genesis_hash(None).unwrap_err().to_string();
    assert!(err.contains("missing genesis block hash"));
}

#[test]
fn test_build_peer_map_prefers_static_address() {
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();
    let pk_bytes = commonware_codec::Encode::encode(&pk);

    let static_addr: std::net::SocketAddr = "10.0.0.1:30400".parse().unwrap();
    let bootnode_addr: std::net::SocketAddr = "192.168.1.1:30400".parse().unwrap();
    let mut bootnode_map = std::collections::BTreeMap::new();
    bootnode_map.insert(pk_bytes.to_vec(), bootnode_addr);

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk.clone()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Known(
            commonware_p2p::Address::Symmetric(static_addr),
        )],
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 1);
    assert_eq!(
        peer_map.get_value(&pk),
        Some(&commonware_p2p::Address::Symmetric(static_addr))
    );
}

#[test]
fn test_build_peer_map_excludes_invalid_registry_without_bootnode_fallback() {
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();
    let pk_bytes = commonware_codec::Encode::encode(&pk);

    let bootnode_addr: std::net::SocketAddr = "192.168.1.1:30400".parse().unwrap();
    let mut bootnode_map = std::collections::BTreeMap::new();
    bootnode_map.insert(pk_bytes.to_vec(), bootnode_addr);

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Invalid],
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 0);
}

#[test]
fn test_build_peer_map_supports_asymmetric_registry_address() {
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();
    let ingress: std::net::SocketAddr = "10.0.0.1:30400".parse().unwrap();
    let egress: std::net::SocketAddr = "10.0.0.2:30401".parse().unwrap();
    let address = commonware_p2p::Address::Asymmetric {
        ingress: commonware_p2p::Ingress::Socket(ingress),
        egress,
    };

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk.clone()],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Known(address.clone())],
    };

    let peer_map = build_peer_map(&validator_set, &std::collections::BTreeMap::new());
    assert_eq!(peer_map.get_value(&pk), Some(&address));
}

#[test]
fn test_build_peer_map_excludes_unreachable() {
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let pk = key.public_key();

    // No p2p_address and no bootnode entry -> excluded.
    let bootnode_map = std::collections::BTreeMap::new();

    let validator_set = validators::ValidatorSet {
        public_keys: vec![pk],
        addresses: vec![Address::with_last_byte(0x01)],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing],
    };

    let peer_map = build_peer_map(&validator_set, &bootnode_map);
    assert_eq!(peer_map.len(), 0);
}

// ---------------------------------------------------------------------------
// BUG-B regression: telemetry label charset (real validator, not source scan).
// ---------------------------------------------------------------------------

/// commonware 2026.5.0's `validate_label` panics if a span/metric label is not
/// `[a-zA-Z][a-zA-Z0-9_]*`. The `with_label` -> `.child()` migration carried
/// dotted labels `dkg.live`/`dkg.retry`, which panicked at block ~90 during DKG
/// rotation - a rare path no short localnet hits. This feeds the labels the
/// engine passes to `Context::child(...)` through the REAL commonware validator
/// (the same function the runtime invokes), so an invalid label fails here
/// instead of in production. Asserts real label values via the real validator;
/// it does NOT scan source text.
///
/// Add new labels here when introducing a labeled child context. New labels are
/// additionally caught at runtime (commonware panics) by the localnet harness,
/// which spawns the `dkg_retry`/`dkg_live` contexts during epoch rotation.
const ENGINE_SPAWN_LABELS: &[&str] = &[
    "application",
    "broadcast",
    "cert_mux",
    "dkg_live",
    "dkg_mux",
    "dkg_retry",
    "engine",
    "executor",
    "finalization",
    "marshal",
    "marshal_blocks",
    "marshal_finalizations",
    "marshal_resolver",
    "network",
    "network_owner",
    "peer_manager",
    "receiver_mux",
    "recovery_blocks",
    "recovery_finalizations",
    "recovery_marshal",
    "res_mux",
    "resolver_handler",
    "sender_mux",
    "vote_mux",
];

#[test]
fn engine_spawn_labels_pass_commonware_validate_label() {
    for label in ENGINE_SPAWN_LABELS {
        commonware_runtime::telemetry::metrics::validate_label(label);
    }
}

/// Guard the guard: prove `validate_label` actually rejects the dotted form that
/// caused BUG-B, so the test above is meaningful (not a no-op validator).
#[test]
#[should_panic]
fn dotted_label_is_rejected_by_commonware_validate_label() {
    commonware_runtime::telemetry::metrics::validate_label("dkg.live");
}

// ---------------------------------------------------------------------------
// marshal-1 regression: restart-from-finalized monotonicity.
// ---------------------------------------------------------------------------

/// commonware 2026.5.0 `marshal::core::Actor::init` returns `Option<Height>`;
/// stack.rs maps `None` (no durable consensus finalization) -> `Height::zero()`
/// (fresh genesis) and `Some(N)` -> `N`. A mis-mapped `None` (e.g.
/// `unwrap_or(nonzero)`) would compile clean but reset a restarted node toward
/// genesis. This pins the mapping contract.
#[test]
fn marshal_init_option_height_maps_none_to_genesis_zero() {
    // Exercise the PRODUCTION mapping (super::map_marshal_init_height), not stdlib
    // Option::unwrap_or - so a regression in how Actor::init's Option<Height> is
    // mapped (e.g. mapping None to a non-zero height, or dropping Some(n)) fails here.
    assert_eq!(super::map_marshal_init_height(None).get(), 0);
    assert_eq!(
        super::map_marshal_init_height(Some(Height::new(7))).get(),
        7
    );
    assert_eq!(
        super::map_marshal_init_height(Some(Height::zero())).get(),
        0
    );
}

// ---------------------------------------------------------------------------
// Block-timing genesis reader / validation (Phase 0/3 of min-block-time).
// ---------------------------------------------------------------------------

/// Test 8: absent genesis key falls back to the supplied default.
#[test]
fn read_ms_uses_default_when_absent() {
    assert_eq!(
        read_ms::<String>(None, "minBlockTimeMs", 2000).unwrap(),
        2000
    );
    assert_eq!(
        read_ms::<String>(None, "leaderTimeoutMs", 4000).unwrap(),
        4000
    );
    assert_eq!(
        read_ms::<String>(None, "certificationTimeoutMs", 8000).unwrap(),
        8000
    );
}

/// Test 9: a present value is returned verbatim (including 0 - the value is read
/// here; the `> 0` rule is enforced by `validate_timing`, see Test 11).
#[test]
fn read_ms_accepts_present_value() {
    assert_eq!(
        read_ms::<String>(Some(Ok(0)), "minBlockTimeMs", 2000).unwrap(),
        0
    );
    assert_eq!(
        read_ms::<String>(Some(Ok(1500)), "minBlockTimeMs", 2000).unwrap(),
        1500
    );
}

/// Test 10: a malformed value surfaces a structured error naming the key.
#[test]
fn read_ms_reports_malformed_value() {
    let err = read_ms(
        Some(Err("expected u64".to_string())),
        "minBlockTimeMs",
        2000,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("invalid genesis config minBlockTimeMs"),
        "error: {err}"
    );
}

/// Test 11: the startup invariants reject 0, min>=leader, and leader>cert.
#[test]
fn validate_timing_rejects_invalid_combinations() {
    let zero = validate_timing(0, 4000, 8000).unwrap_err().to_string();
    assert!(zero.contains("minBlockTimeMs"), "error: {zero}");
    assert!(validate_timing(4000, 4000, 8000).is_err()); // min == leader
    assert!(validate_timing(5000, 4000, 8000).is_err()); // min > leader
    assert!(validate_timing(2000, 9000, 8000).is_err()); // leader > cert
}

/// Test 12: the shipped defaults satisfy `0 < min < leader <= cert`.
#[test]
fn validate_timing_accepts_defaults() {
    assert!(validate_timing(2000, 4000, 8000).is_ok());
}

#[test]
fn ocomp_manifest_hash_separates_p2p_before_consensus_participation() {
    let legacy = ocomp_p2p_namespace(None);
    let first = ocomp_p2p_namespace(Some(B256::repeat_byte(0x11)));
    let replay = ocomp_p2p_namespace(Some(B256::repeat_byte(0x11)));
    let different = ocomp_p2p_namespace(Some(B256::repeat_byte(0x12)));

    assert_eq!(first, replay);
    assert_ne!(first, legacy);
    assert_ne!(first, different);
}
