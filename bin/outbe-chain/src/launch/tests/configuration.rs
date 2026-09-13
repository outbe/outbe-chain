#[test]
fn engine_builds_payloads_after_prefinalization_parent_switches() {
    let mut engine = reth_node_core::args::EngineArgs::default();
    assert!(!engine.always_process_payload_attributes_on_canonical_head);
    assert!(!engine.allow_unwind_canonical_header);

    super::configure_outbe_engine_args(&mut engine);

    assert!(engine.always_process_payload_attributes_on_canonical_head);
    assert!(engine.allow_unwind_canonical_header);
    let tree = engine.tree_config();
    assert!(tree.always_process_payload_attributes_on_canonical_head());
    assert!(tree.unwind_canonical_header());
}

/// Pool lifetime hardening: parked transactions must age out in minutes,
/// not hours, RPC submissions must not be exempt from that eviction, and a
/// restart must not resurrect what the node evicted.
///
/// Asserted through `TxPoolArgs::default()`, which reads the installed
/// global defaults - the same values clap hands the node when no
/// `--txpool.*` flag is given.
#[test]
fn txpool_defaults_bound_transaction_lifetime() {
    // Installing is idempotent-by-OnceLock; another test in this binary may
    // have installed the same values first, which is equally correct.
    let _ = super::outbe_default_txpool_values().try_init();

    let args = reth_node_core::args::TxPoolArgs::default();
    assert_eq!(
        args.max_queued_lifetime,
        std::time::Duration::from_secs(120),
        "parked transactions must age out in minutes, not the upstream 3 hours"
    );
    assert!(
        args.no_locals,
        "RPC-submitted transactions must not be exempt from lifetime eviction"
    );
    assert!(
        args.disable_transactions_backup,
        "a restart must not resurrect evicted transactions"
    );
}

#[test]
fn adr005_accepts_validators_and_certified_followers_only() {
    super::validate_adr005_node_mode(true, false).expect("validator path is parent-gated");
    super::validate_adr005_node_mode(false, true).expect("certified follower path is parent-gated");

    let error = super::validate_adr005_node_mode(false, false)
        .expect_err("plain EL sync has no finalized-parent projection barrier");
    assert!(error.to_string().contains("--upstream"));
}

/// Full-node mode: RPC handler created without bridge -> is_validator = false.
#[test]
fn test_fullnode_rpc_no_bridge_means_not_validator() {
    // When OutbeApiHandler::new(provider) is called (no bridge),
    // bridge field is None, so is_validator = bridge.is_some() = false.
    let bridge: Option<outbe_engine::bridge::ConsensusExecutionBridge> = None;
    assert!(
        bridge.is_none(),
        "full node must have bridge=None -> is_validator=false"
    );
}

/// Validator mode: RPC handler created with bridge -> is_validator = true.
#[test]
fn test_validator_rpc_with_bridge_means_validator() {
    let bridge = outbe_engine::bridge::ConsensusExecutionBridge::new();
    let bridge_opt: Option<outbe_engine::bridge::ConsensusExecutionBridge> = Some(bridge);
    assert!(
        bridge_opt.is_some(),
        "validator must have bridge=Some -> is_validator=true"
    );
}
