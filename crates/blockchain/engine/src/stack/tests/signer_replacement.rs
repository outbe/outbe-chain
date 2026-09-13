use super::*;

#[test]
fn signer_replacement_aborts_engine_after_epoch_select_completes() {
    commonware_tokio::Runner::default().start(|context| async move {
        let mut engine_handle = context
            .child("replace_signer_engine")
            .spawn(|engine| async move {
                let _ = engine.stopped().await;
            });

        let action = supervise_epoch_loop_result(
            &context,
            Ok(EpochLoopOutcome::ReplaceSigner),
            &mut engine_handle,
            &crate::application_shutdown::ApplicationDrain::default(),
        )
        .await
        .expect("signer replacement must stop the old engine before restarting the epoch");

        assert_eq!(action, EpochLoopAction::ReplaceSigner);
    });
}

#[test]
fn radicle_gate_controls_signing() {
    assert!(!radicle_signer_enabled(RadicleVotingGate::Verifier, true).unwrap());
    assert!(radicle_signer_enabled(RadicleVotingGate::SignerAllowed, true).unwrap());
    assert!(!radicle_signer_enabled(RadicleVotingGate::SignerAllowed, false).unwrap());

    let error = radicle_signer_enabled(
        RadicleVotingGate::Fatal(RadicleVotingGateError::BindingMismatch),
        true,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("binding mismatch"), "error: {error}");
}

#[tokio::test]
async fn radicle_fatal_notifies_epoch() {
    let (publisher, handle) = RadicleStatusChannel::enabled(Address::repeat_byte(1), [1_u8; 32]);
    let mut updates = handle.subscribe();
    let waiter =
        tokio::spawn(async move { wait_for_radicle_role_change(&mut updates, false, true).await });
    publisher.set_voting_gate(RadicleVotingGate::Fatal(
        RadicleVotingGateError::BindingMismatch,
    ));
    let error = tokio::time::timeout(Duration::from_millis(20), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("binding mismatch"), "error: {error}");
}

#[tokio::test]
async fn canonical_signer_permission_wakes_a_running_verifier_epoch() {
    let (publisher, handle) = RadicleStatusChannel::enabled(Address::repeat_byte(1), [1_u8; 32]);
    let mut updates = handle.subscribe();
    let waiter =
        tokio::spawn(async move { wait_for_radicle_role_change(&mut updates, false, true).await });

    // The epoch was built as a verifier while the canonical ValidatorSet still
    // classified the joining node as pending. Once the next finalized block
    // admits that node, the lifecycle must wake so the already-loaded share can
    // be installed in a replacement engine for the same epoch.
    publisher.set_voting_gate(RadicleVotingGate::SignerAllowed);

    let desired_signer = tokio::time::timeout(Duration::from_millis(20), waiter)
        .await
        .expect("SignerAllowed must wake a running verifier epoch")
        .expect("role watcher task must not panic")
        .expect("SignerAllowed is a non-fatal lifecycle transition");
    assert!(desired_signer);
}

#[tokio::test]
async fn signer_permission_without_a_share_does_not_wake_a_verifier_epoch() {
    let (publisher, handle) = RadicleStatusChannel::enabled(Address::repeat_byte(1), [1_u8; 32]);
    let mut updates = handle.subscribe();
    let mut waiter =
        tokio::spawn(async move { wait_for_radicle_role_change(&mut updates, false, false).await });

    publisher.set_voting_gate(RadicleVotingGate::SignerAllowed);

    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut waiter)
            .await
            .is_err(),
        "SignerAllowed must not manufacture a missing threshold share"
    );
    waiter.abort();
}

#[tokio::test]
async fn canonical_verifier_gate_wakes_a_running_signer_epoch() {
    let (publisher, handle) = RadicleStatusChannel::enabled(Address::repeat_byte(1), [1_u8; 32]);
    publisher.set_voting_gate(RadicleVotingGate::SignerAllowed);
    let mut updates = handle.subscribe();
    let waiter =
        tokio::spawn(async move { wait_for_radicle_role_change(&mut updates, true, true).await });

    publisher.set_voting_gate(RadicleVotingGate::Verifier);

    let desired_signer = tokio::time::timeout(Duration::from_millis(20), waiter)
        .await
        .expect("Verifier must wake a running signer epoch")
        .expect("role watcher task must not panic")
        .expect("Verifier is a non-fatal lifecycle transition");
    assert!(!desired_signer);
}

#[test]
fn test_recovered_boundary_evm_signer_authorization_survives_latest_state_removal() {
    use crate::args::ConsensusArgs;
    use commonware_cryptography::Signer as _;
    use std::net::SocketAddr;

    let temp = tempfile::tempdir().unwrap();
    let evm_key_path = temp.path().join("evm-key.hex");
    let evm_secret = [0x42u8; 32];
    std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let evm_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();

    let (keys, participants, output, _polynomial) = run_test_dkg();
    let local_key = &keys[0];
    let boundary_addresses = vec![
        evm_signer.address(),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let boundary_validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let boundary = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &boundary_validator_set,
        output: &output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 10,
        planned_activation_height: 20,
        vrf_material_version: 2,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();

    let latest_after_unfinalized_removal = validators::ValidatorSet {
        public_keys: keys.iter().skip(1).map(|k| k.public_key()).collect(),
        addresses: boundary_addresses.iter().skip(1).copied().collect(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 2],
    };
    let args = ConsensusArgs {
        is_validator: true,
        signing_key: Some(temp.path().join("signing-key.hex")),
        validator_evm_key: Some(evm_key_path.clone()),
        signing_share: None,
        public_polynomial: None,
        dkg_output: None,
        listen_address: "127.0.0.1:30400".parse::<SocketAddr>().unwrap(),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        tee_enclave_socket: None,
        tee_session_mode: crate::args::TeeSessionMode::PolicyDefault,
        tee_bootstrap_timeout_secs: 60,
        tee_canary_interval_secs: 30,
        tee_canary_failure_threshold: 3,
        txpool_pending_staleness_secs: 600,
        radicle_control_socket: None,
        radicle_status_address: None,
        upstream: None,
        upstream_nocertify: false,
        projection_storage_config: Some("/tmp/offchain-storage.toml".into()),
    };

    let address = validate_validator_evm_signer(
        &args,
        local_key,
        &latest_after_unfinalized_removal,
        &latest_after_unfinalized_removal,
        Some((&participants, &boundary)),
        false,
    )
    .unwrap();
    assert_eq!(address, Some(evm_signer.address()));

    let wrong_key_path = temp.path().join("wrong-evm-key.hex");
    let wrong_secret = [0x43u8; 32];
    std::fs::write(&wrong_key_path, hex::encode(wrong_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrong_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let wrong_args = ConsensusArgs {
        validator_evm_key: Some(wrong_key_path),
        ..args
    };
    let err = validate_validator_evm_signer(
        &wrong_args,
        local_key,
        &latest_after_unfinalized_removal,
        &latest_after_unfinalized_removal,
        Some((&participants, &boundary)),
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("does not match recovered DKG boundary address"));
}

#[test]
fn test_register_epoch_validation_providers_is_available_and_first_wins() {
    let (keys, participants, _output, polynomial) = run_test_dkg();
    let vrf_materials = VrfMaterialProvider::new(0, polynomial, None);
    let epoch = Epoch::new(9);
    let validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x01),
            Address::with_last_byte(0x02),
            Address::with_last_byte(0x03),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let expected_committee = ordered_validator_addresses(&participants, &validator_set).unwrap();
    let scheme_provider = HybridSchemeProvider::new();
    let committee_provider = CommitteeProvider::new();

    register_epoch_validation_providers(
        epoch,
        &participants,
        &validator_set,
        None,
        &vrf_materials,
        &scheme_provider,
        &committee_provider,
    )
    .unwrap();

    assert!(scheme_provider.scoped(epoch).is_some());
    assert_eq!(
        committee_provider
            .ordered_committee(epoch)
            .expect("committee should be registered")
            .as_ref(),
        &expected_committee
    );

    let replacement_set = validators::ValidatorSet {
        public_keys: validator_set.public_keys.clone(),
        addresses: vec![
            Address::with_last_byte(0xAA),
            Address::with_last_byte(0xBB),
            Address::with_last_byte(0xCC),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    register_epoch_validation_providers(
        epoch,
        &participants,
        &replacement_set,
        None,
        &vrf_materials,
        &scheme_provider,
        &committee_provider,
    )
    .unwrap();

    assert_eq!(
        committee_provider
            .ordered_committee(epoch)
            .expect("committee should remain registered")
            .as_ref(),
        &expected_committee
    );
}

#[test]
fn evm_signer_validation_allows_active_validator_waiting_for_live_join_share() {
    use crate::args::ConsensusArgs;
    use crate::validators::{ValidatorP2pAddress, ValidatorSet};
    use commonware_cryptography::Signer as _;
    use std::net::SocketAddr;

    let temp = tempfile::tempdir().unwrap();
    let evm_key_path = temp.path().join("evm-key.hex");
    let evm_secret = [0x11u8; 32];
    std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let evm_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();
    let bls_key = bls12381::PrivateKey::from_seed(7);
    let consensus_set = ValidatorSet {
        public_keys: Vec::new(),
        addresses: Vec::new(),
        p2p_addresses: Vec::new(),
    };
    let active_set = ValidatorSet {
        public_keys: vec![bls_key.public_key()],
        addresses: vec![evm_signer.address()],
        p2p_addresses: vec![ValidatorP2pAddress::Missing],
    };
    let args = ConsensusArgs {
        is_validator: true,
        signing_key: Some(temp.path().join("signing-key.hex")),
        validator_evm_key: Some(evm_key_path),
        signing_share: None,
        public_polynomial: None,
        dkg_output: None,
        listen_address: "127.0.0.1:30400".parse::<SocketAddr>().unwrap(),
        storage_dir: None,
        keys_dir: None,
        trust_el_head: false,
        testnet_unix_time_offset_secs: None,
        consensus_peers: Vec::new(),
        use_local_defaults: true,
        payload_resolve_time_ms: 200,
        payload_return_time_ms: 450,
        worker_threads: 1,
        bls_key_backend: "plaintext".to_string(),
        bls_passphrase: None,
        tee_enclave_socket: None,
        tee_session_mode: crate::args::TeeSessionMode::PolicyDefault,
        tee_bootstrap_timeout_secs: 60,
        tee_canary_interval_secs: 30,
        tee_canary_failure_threshold: 3,
        txpool_pending_staleness_secs: 600,
        radicle_control_socket: None,
        radicle_status_address: None,
        upstream: None,
        upstream_nocertify: false,
        projection_storage_config: Some("/tmp/offchain-storage.toml".into()),
    };

    let address = super::validate_validator_evm_signer(
        &args,
        &bls_key,
        &consensus_set,
        &active_set,
        None,
        false,
    )
    .unwrap();

    assert_eq!(address, Some(evm_signer.address()));

    // Verifier-join: an EVM signer NOT in either set must NOT bail when verifier_join
    // is true - it returns None (the node syncs as a verifier). The same signer with
    // verifier_join=false bails (the existing member-required contract).
    let empty = crate::validators::ValidatorSet {
        public_keys: Vec::new(),
        addresses: Vec::new(),
        p2p_addresses: Vec::new(),
    };
    assert!(
        super::validate_validator_evm_signer(&args, &bls_key, &empty, &empty, None, false).is_err(),
        "non-member must bail when not verifier-join"
    );
    assert_eq!(
        super::validate_validator_evm_signer(&args, &bls_key, &empty, &empty, None, true).unwrap(),
        None,
        "non-member must run as verifier (None) when verifier-join"
    );

    // Lease recovery crosses a different boundary from an unregistered live join:
    // the old finalized DKG committee excludes this validator, while the canonical
    // reshare target already binds its EVM address to the same BLS key. Keep that
    // identity available for the post-DKG signer transition, but do not infer any
    // threshold authority from it (the runtime still has no signing share).
    let (boundary_keys, boundary_participants, boundary_output, _polynomial) = run_test_dkg();
    let boundary_set = ValidatorSet {
        public_keys: boundary_keys.iter().map(|key| key.public_key()).collect(),
        addresses: vec![
            Address::with_last_byte(0x31),
            Address::with_last_byte(0x32),
            Address::with_last_byte(0x33),
        ],
        p2p_addresses: vec![ValidatorP2pAddress::Missing; 3],
    };
    let boundary = dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(7),
        validator_set: &boundary_set,
        output: &boundary_output,
        is_full_dkg: false,
        dkg_cycle: 6,
        freeze_height: 420,
        planned_activation_height: 421,
        vrf_material_version: 7,
        is_validator_set_change: true,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap();

    assert_eq!(
        super::validate_validator_evm_signer(
            &args,
            &bls_key,
            &boundary_set,
            &active_set,
            Some((&boundary_participants, &boundary)),
            true,
        )
        .unwrap(),
        Some(evm_signer.address()),
        "a shareless validator in the canonical reshare target must retain its identity for post-DKG promotion"
    );

    let mismatched_target = ValidatorSet {
        public_keys: vec![bls12381::PrivateKey::from_seed(8).public_key()],
        addresses: vec![evm_signer.address()],
        p2p_addresses: vec![ValidatorP2pAddress::Missing],
    };
    let mismatch = super::validate_validator_evm_signer(
        &args,
        &bls_key,
        &boundary_set,
        &mismatched_target,
        Some((&boundary_participants, &boundary)),
        true,
    )
    .unwrap_err()
    .to_string();
    assert!(
        mismatch.contains("belongs to a different BLS consensus key"),
        "shareless recovery must fail closed on an address/BLS mismatch: {mismatch}"
    );

    assert_eq!(
        super::validate_validator_evm_signer(
            &args,
            &bls_key,
            &boundary_set,
            &empty,
            Some((&boundary_participants, &boundary)),
            true,
        )
        .unwrap(),
        None,
        "an unregistered shareless verifier has no canonical proposer identity"
    );
    assert!(
        super::validate_validator_evm_signer(
            &args,
            &bls_key,
            &boundary_set,
            &active_set,
            Some((&boundary_participants, &boundary)),
            false,
        )
        .is_err(),
        "an excluded validator must fail closed outside shareless recovery mode"
    );
}
