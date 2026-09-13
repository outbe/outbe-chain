use super::*;

#[derive(Debug)]
struct RecordingCeStartupRecovery {
    requested_height: AtomicU64,
    marker: outbe_compressed_entities::FinalizedMarker,
}

impl CeStartupRecovery for RecordingCeStartupRecovery {
    fn recover_before_participation(
        &self,
        consensus_finalized_height: u64,
    ) -> std::result::Result<
        outbe_compressed_entities::FinalizedMarker,
        crate::ce_recovery::CeStartupRecoveryError,
    > {
        self.requested_height
            .store(consensus_finalized_height, Ordering::SeqCst);
        Ok(self.marker)
    }
}

#[test]
fn ce_recovery_uses_exact_archive_backed_head_when_ack_floor_lags() {
    let archive_height = 302;
    let marshal_processed_height = 301;
    let archive_hash = B256::repeat_byte(0x42);
    let round = Round::new(Epoch::new(3), View::new(17));
    let (recovery_anchor_height, _, _) = reconcile_recovered_execution_head(
        archive_height,
        archive_hash,
        Some(RecoveredApplicationFinalization {
            round,
            digest: Digest(archive_hash),
        }),
    )
    .unwrap();
    let marker = outbe_compressed_entities::FinalizedMarker {
        commitment_scheme_version: 1,
        height: archive_height,
        block_hash: archive_hash,
        parent_block_hash: B256::repeat_byte(0x41),
        parent_root: B256::repeat_byte(0x51),
        new_root: B256::repeat_byte(0x52),
    };
    let recovery = RecordingCeStartupRecovery {
        requested_height: AtomicU64::new(u64::MAX),
        marker,
    };

    let recovered = recover_ce_at_reconciled_anchor(
        &recovery,
        marshal_processed_height,
        recovery_anchor_height,
    )
    .unwrap();

    assert_eq!(recovered, marker);
    assert_eq!(
        recovery.requested_height.load(Ordering::SeqCst),
        archive_height,
        "CE recovery must use exact archived finality, not the lagging ACK floor"
    );
}

#[test]
fn benign_unfinalized_head_lead_is_recoverable() {
    // Steady state: head is exactly one block ahead of the finalized tip.
    assert!(unfinalized_head_lead_is_recoverable(70, 69));
    // A few blocks ahead during a finalization hiccup, up to the bound.
    assert!(unfinalized_head_lead_is_recoverable(
        69 + MAX_UNFINALIZED_HEAD_LEAD,
        69
    ));
}

#[test]
fn recovery_anchor_never_promotes_an_execution_only_head_to_finalized() {
    assert_eq!(durable_recovery_anchor_height(70, 69), 69);
    assert_eq!(durable_recovery_anchor_height(69, 69), 69);
    assert_eq!(durable_recovery_anchor_height(68, 69), 68);
    assert_eq!(durable_recovery_anchor_height(0, 0), 0);
}

#[test]
fn no_lead_is_not_a_recovery_case() {
    // head == finalized: recover(head) would have succeeded; not this arm.
    assert!(!unfinalized_head_lead_is_recoverable(69, 69));
    // head behind finalized (execution lags): saturating lead is 0.
    assert!(!unfinalized_head_lead_is_recoverable(68, 69));
}

#[test]
fn zero_finalized_tip_is_not_recoverable() {
    // No durable finalized tip at all -> fresh/corrupt, never the benign case.
    assert!(!unfinalized_head_lead_is_recoverable(5, 0));
}

#[test]
fn lead_beyond_bound_stays_fatal() {
    // A head far ahead of the finalized tip is suspicious, not an in-flight
    // head - it must NOT be silently tolerated.
    assert!(!unfinalized_head_lead_is_recoverable(
        69 + MAX_UNFINALIZED_HEAD_LEAD + 1,
        69
    ));
}

#[test]
fn bounded_head_lead_membership_drift_uses_recovered_boundary_committee() {
    use commonware_cryptography::Signer as _;
    use std::net::SocketAddr;

    let marshal_finalized_height = 100;
    let reth_head = marshal_finalized_height + MAX_UNFINALIZED_HEAD_LEAD;
    assert!(
        unfinalized_head_lead_is_recoverable(reth_head, marshal_finalized_height),
        "bounded Reth head lead should be treated as the benign restart window"
    );

    let temp = tempfile::tempdir().unwrap();
    let evm_key_path = temp.path().join("evm-key.hex");
    let evm_secret = [0x52u8; 32];
    std::fs::write(&evm_key_path, hex::encode(evm_secret)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&evm_key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let evm_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes(evm_secret).unwrap();

    let (keys, _participants, output, polynomial) = run_test_dkg();
    let local_key = &keys[0];
    let boundary_addresses = vec![
        evm_signer.address(),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x33),
    ];
    let boundary_validator_set = validators::ValidatorSet {
        public_keys: keys.iter().map(|key| key.public_key()).collect(),
        addresses: boundary_addresses.clone(),
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    let recovered_boundary =
        dkg_manager::build_boundary_artifact(dkg_manager::BoundaryArtifactInput {
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
    let boundary_participants =
        select_recovery_participants(output.players(), &recovered_boundary).unwrap();
    assert_eq!(&boundary_participants, output.players());

    // Simulate provider-latest state after an unfinalized membership-changing
    // head: old participant A has been removed, and a new D is present.
    let replacement_key = bls12381::PrivateKey::from_seed(99);
    let latest_after_unfinalized_removal = validators::ValidatorSet {
        public_keys: vec![
            keys[1].public_key(),
            keys[2].public_key(),
            replacement_key.public_key(),
        ],
        addresses: vec![
            Address::with_last_byte(0x22),
            Address::with_last_byte(0x33),
            Address::with_last_byte(0x44),
        ],
        p2p_addresses: vec![validators::ValidatorP2pAddress::Missing; 3],
    };
    assert!(
        ordered_validator_addresses(&boundary_participants, &latest_after_unfinalized_removal)
            .is_err(),
        "pre-fix provider-latest address mapping should fail when old A is absent"
    );

    let vrf_materials = VrfMaterialProvider::new(2, polynomial, None);
    let (_verifier_scheme, recovered_addresses) = epoch_validation_inputs(
        Epoch::new(7),
        &boundary_participants,
        &latest_after_unfinalized_removal,
        Some(&recovered_boundary),
        &vrf_materials,
    )
    .expect("bounded-head-lead recovery must use recovered boundary committee");
    assert_eq!(recovered_addresses, boundary_addresses);

    let args = crate::args::ConsensusArgs {
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
    let signer_address = validate_validator_evm_signer(
        &args,
        local_key,
        &latest_after_unfinalized_removal,
        &latest_after_unfinalized_removal,
        Some((&boundary_participants, &recovered_boundary)),
        false,
    )
    .expect("old-epoch signer A should be authorized by recovered boundary, not latest state");
    assert_eq!(signer_address, Some(evm_signer.address()));
}
