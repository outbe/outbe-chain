use super::*;

#[test]
fn measurement_transition_reuses_the_finalized_candidate_workflow() {
    let fixture =
        replacement_fixture_for_operation(AttestationOperationV1::TransitionEnclaveMeasurement);
    let submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap();
    let AttestationEvidenceV1::Dcap(evidence) = evidence else {
        panic!("expected DCAP transition evidence")
    };
    assert_eq!(
        evidence.intent.operation,
        AttestationOperationV1::TransitionEnclaveMeasurement
    );
    assert_eq!(evidence.intent.transition_nonce, 1);
    assert_eq!(
        promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
        fixture.candidate
    );
}

#[test]
fn expired_rejoin_registration_reuses_the_finalized_candidate_workflow() {
    let fixture = replacement_fixture_for_operation(AttestationOperationV1::RegisterEnclave);
    let submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let mut expected_submission = vec![1];
    expected_submission.extend_from_slice(
        &u32::try_from(submission.evidence().len())
            .unwrap()
            .to_be_bytes(),
    );
    expected_submission.extend_from_slice(submission.evidence());
    expected_submission.extend_from_slice(submission.node_signature());
    expected_submission.extend_from_slice(submission.enclave_signature());
    assert_eq!(
        std::fs::read(&fixture.paths.replacement_submission).unwrap(),
        expected_submission
    );
    let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap();
    let AttestationEvidenceV1::Dcap(evidence) = evidence else {
        panic!("expected DCAP registration evidence")
    };
    assert_eq!(
        evidence.intent.operation,
        AttestationOperationV1::RegisterEnclave
    );
    let raw_transaction = vec![0x91, 0x92, 0x93];
    let relay = persist_replacement_candidate_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x90),
        &raw_transaction,
    )
    .unwrap();
    let mut expected_relay = vec![1];
    expected_relay.extend_from_slice(submission.submission_hash().unwrap().as_slice());
    expected_relay.extend_from_slice(B256::repeat_byte(0x90).as_slice());
    expected_relay.extend_from_slice(keccak256(&raw_transaction).as_slice());
    expected_relay.extend_from_slice(&u32::try_from(raw_transaction.len()).unwrap().to_be_bytes());
    expected_relay.extend_from_slice(&raw_transaction);
    assert_eq!(
        std::fs::read(&fixture.paths.replacement_relay).unwrap(),
        expected_relay
    );
    assert_eq!(relay.transaction_hash(), keccak256(&raw_transaction));
    assert_eq!(
        persist_replacement_candidate_relay(
            &fixture.node_data_dir,
            B256::repeat_byte(0x90),
            &raw_transaction,
        )
        .unwrap(),
        relay
    );
    assert!(persist_replacement_candidate_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x90),
        &[0x94],
    )
    .unwrap_err()
    .to_string()
    .contains("conflicts"));
    assert_eq!(
        load_replacement_candidate_relay(&fixture.node_data_dir)
            .unwrap()
            .unwrap(),
        relay
    );
    assert_eq!(
        promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
        fixture.candidate
    );
    assert!(load_replacement_candidate_relay(&fixture.node_data_dir)
        .unwrap()
        .is_none());
}

#[test]
fn replacement_record_decode_preserves_first_error() {
    let fixture = replacement_fixture_for_operation(AttestationOperationV1::RegisterEnclave);
    persist_replacement_candidate_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x81),
        &[0x82, 0x83],
    )
    .unwrap();
    let submission = std::fs::read(&fixture.paths.replacement_submission).unwrap();
    let relay = std::fs::read(&fixture.paths.replacement_relay).unwrap();

    let mut invalid_frame = submission.clone();
    invalid_frame[0] = 2;
    invalid_frame[1..5].fill(0);
    std::fs::write(&fixture.paths.replacement_submission, invalid_frame).unwrap();
    assert_eq!(
        read_replacement_submission(&fixture.paths.replacement_submission)
            .unwrap_err()
            .to_string(),
        "codec error: replacement submission framing is invalid"
    );

    let mut invalid_length = submission;
    invalid_length[1..5].fill(0);
    invalid_length[5] = 0xff;
    std::fs::write(&fixture.paths.replacement_submission, invalid_length).unwrap();
    assert_eq!(
        read_replacement_submission(&fixture.paths.replacement_submission)
            .unwrap_err()
            .to_string(),
        "codec error: replacement submission evidence length is non-canonical"
    );

    super::fixtures::assert_relay_decode_precedence(
        &fixture.paths,
        &relay,
        super::fixtures::RelayRecordKind::Replacement,
    )
    .unwrap();
}

#[test]
fn development_expired_rejoin_uses_the_same_durable_promotion_journal() {
    let DirectDevRegistration {
        fixture, evidence, ..
    } = direct_dev_registration();
    assert!(matches!(
        evidence,
        AttestationEvidenceV1::GramineDirectDev(_)
    ));
    assert_eq!(
        promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
        fixture.candidate
    );
}

#[test]
fn restart_commits_only_the_exact_fsynced_candidate_relay_checkpoint() {
    let fixture = replacement_fixture_for_operation(AttestationOperationV1::RegisterEnclave);
    let relay = persist_replacement_candidate_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x81),
        &[0x82, 0x83],
    )
    .unwrap();
    let bytes = stage_existing_record_as_next(
        &fixture.paths.replacement_relay,
        &fixture.paths.replacement_relay_next,
        &fixture.paths.root,
        DirectorySync::Sync,
    );

    assert_eq!(
        load_replacement_candidate_relay(&fixture.node_data_dir)
            .unwrap()
            .unwrap(),
        relay
    );
    assert!(fixture.paths.replacement_relay.exists());
    assert!(!fixture.paths.replacement_relay_next.exists());

    let mut corrupt = bytes;
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(&fixture.paths.replacement_relay, corrupt).unwrap();
    assert!(load_replacement_candidate_relay(&fixture.node_data_dir).is_err());
}

#[test]
fn finalized_exact_candidate_promotes_atomically_and_idempotently() {
    let fixture = replacement_fixture();
    assert_ne!(fixture.active, fixture.candidate);

    assert_eq!(
        promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
        fixture.candidate
    );
    let wrong_completed_intent = FinalizedReplacementAuthorizationV1::for_test(
        B256::repeat_byte(0x93),
        fixture.authorization.candidate_manifest_hash,
    );
    assert!(
        promote_replacement_candidate(&fixture.node_data_dir, &wrong_completed_intent)
            .unwrap_err()
            .to_string()
            .contains("completed promotion authorization does not match")
    );
    assert_eq!(
        promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap(),
        fixture.candidate
    );
    assert!(!fixture.paths.replacement_candidate.exists());
    assert!(!fixture.paths.replacement_submission.exists());
    assert_eq!(
        NodeHostNoiseKey::load(&fixture.paths.noise_key)
            .unwrap()
            .public(),
        fixture.node_host_public
    );
    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.candidate
    );
}

fn assert_committed_identity_loaders(
    fixture: &ReplacementFixture,
    expected: &EnclaveInitializationManifestV1,
) {
    assert_eq!(
        &load_committed_enclave_manifest_v1(&fixture.node_data_dir).unwrap(),
        expected
    );
    let (manifest, key) = committed_node_host_session_material(&fixture.node_data_dir).unwrap();
    assert_eq!(&manifest, expected);
    assert_eq!(key.public(), fixture.node_host_public);
}

fn assert_committed_identity_loader_error(fixture: &ReplacementFixture, expected: &str) {
    assert_eq!(
        load_committed_enclave_manifest_v1(&fixture.node_data_dir)
            .err()
            .unwrap()
            .to_string(),
        expected
    );
    assert_eq!(
        committed_node_host_session_material(&fixture.node_data_dir)
            .err()
            .unwrap()
            .to_string(),
        expected
    );
}

#[test]
fn committed_identity_loaders_read_the_active_manifest_and_key() {
    let fixture = replacement_fixture();
    assert_committed_identity_loaders(&fixture, &fixture.active);

    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();
    assert_committed_identity_loaders(&fixture, &fixture.candidate);
}

#[test]
fn committed_identity_loader_guards_precede_key_reads_and_recovery() {
    let expected = "codec error: one committed production NodeHost manifest is required";

    let missing_manifest = replacement_fixture();
    std::fs::remove_file(&missing_manifest.paths.manifest).unwrap();
    std::fs::write(&missing_manifest.paths.noise_key, b"bad key").unwrap();
    assert_committed_identity_loader_error(&missing_manifest, expected);

    let missing_key = replacement_fixture();
    std::fs::remove_file(&missing_key.paths.noise_key).unwrap();
    assert_committed_identity_loader_error(&missing_key, expected);

    let pending = replacement_fixture();
    write_bytes_once(
        &pending.paths.pending_manifest,
        &pending.active.encode_canonical().unwrap(),
        &pending.paths.root,
    )
    .unwrap();
    std::fs::write(&pending.paths.noise_key, b"bad key").unwrap();
    assert_committed_identity_loader_error(&pending, expected);
}

#[test]
fn committed_identity_loaders_reject_manifest_key_mismatch() {
    let fixture = replacement_fixture();
    std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
    std::fs::remove_file(&fixture.paths.replacement_candidate).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
    let mut manifest = fixture.active.clone();
    manifest.node_host_noise_x25519 = [0x99; 32];
    std::fs::write(
        &fixture.paths.manifest,
        manifest.encode_canonical().unwrap(),
    )
    .unwrap();

    let expected = "codec error: committed manifest does not match the persistent NodeHost key";
    assert_committed_identity_loader_error(&fixture, expected);
}

#[test]
fn production_authority_is_constructed_only_from_the_exact_finalized_replacement_binding() {
    let fixture = replacement_fixture();
    let candidate = read_replacement_candidate(&fixture.paths.replacement_candidate).unwrap();
    let submission = read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
    let intent = validate_durable_replacement_submission(&candidate.manifest, &submission).unwrap();
    let finalized = FinalizedReplacementBindingV1 {
        view: crate::FinalizedRegistryViewV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            block_number: 90,
            block_hash: B256::repeat_byte(0xA1),
            state_root: B256::repeat_byte(0xA2),
            consensus_timestamp: 19_000,
        },
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        intent_hash: intent.intent_hash().unwrap(),
        binding_version: intent.binding_version,
        registration_version: intent.registration_version,
        valid_until: 20_000,
        recipient_x25519: intent.recipient_x25519,
        attestation_ed25519: intent.attestation_ed25519,
        noise_responder_x25519: intent.noise_responder_x25519,
        node_host_authorization_hash: intent.node_host_authorization_hash,
    };

    assert_eq!(
        construct_finalized_replacement_authorization_v1(&fixture.node_data_dir, &finalized,)
            .unwrap(),
        fixture.authorization
    );

    let mut wrong = finalized;
    wrong.intent_hash = B256::repeat_byte(0xA3);
    assert!(
        construct_finalized_replacement_authorization_v1(&fixture.node_data_dir, &wrong,)
            .unwrap_err()
            .to_string()
            .contains("finalized Registry binding does not match")
    );

    let mut wrong_lease = finalized;
    wrong_lease.valid_until -= 1;
    assert!(
        construct_finalized_replacement_authorization_v1(&fixture.node_data_dir, &wrong_lease,)
            .unwrap_err()
            .to_string()
            .contains("finalized Registry binding does not match")
    );
}

#[test]
fn promotion_requires_the_exact_finalized_intent_and_candidate() {
    let fixture = replacement_fixture();
    let wrong_candidate = FinalizedReplacementAuthorizationV1::for_test(
        fixture.authorization.intent_hash,
        B256::repeat_byte(0x91),
    );
    assert!(
        promote_replacement_candidate(&fixture.node_data_dir, &wrong_candidate)
            .unwrap_err()
            .to_string()
            .contains("targets another candidate manifest")
    );

    let wrong_intent = FinalizedReplacementAuthorizationV1::for_test(
        B256::repeat_byte(0x92),
        fixture.authorization.candidate_manifest_hash,
    );
    assert!(
        promote_replacement_candidate(&fixture.node_data_dir, &wrong_intent)
            .unwrap_err()
            .to_string()
            .contains("targets another replacement intent")
    );

    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.active
    );
    assert!(fixture.paths.replacement_candidate.exists());
    assert!(fixture.paths.replacement_submission.exists());
    assert!(!fixture.paths.next_manifest.exists());
}

#[test]
fn restart_rejects_a_candidate_refresh_that_changes_enclave_identity() {
    let fixture = replacement_fixture();
    std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
    let mut candidate = read_replacement_candidate(&fixture.paths.replacement_candidate).unwrap();
    candidate.manifest.recipient_x25519[0] ^= 1;
    write_bytes_once(
        &fixture.paths.replacement_candidate_next,
        &candidate.encode_canonical().unwrap(),
        &fixture.paths.root,
    )
    .unwrap();

    let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
    assert!(reconcile_replacement_state(&fixture.paths, &node_host)
        .unwrap_err()
        .to_string()
        .contains("changes replacement identity"));
    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.active
    );
    assert!(fixture.paths.replacement_candidate_next.exists());
}

#[test]
fn restart_atomically_recovers_candidate_and_submission_and_reloads_exact_bytes() {
    let fixture = replacement_fixture();
    let candidate_record = std::fs::read(&fixture.paths.replacement_candidate).unwrap();
    let submission_record = std::fs::read(&fixture.paths.replacement_submission).unwrap();
    let expected_submission =
        read_replacement_submission(&fixture.paths.replacement_submission).unwrap();
    std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
    std::fs::remove_file(&fixture.paths.replacement_candidate).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();

    write_bytes_once(
        &fixture.paths.replacement_candidate_next,
        &candidate_record,
        &fixture.paths.root,
    )
    .unwrap();
    let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
    reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
    assert!(fixture.paths.replacement_candidate.exists());
    assert!(!fixture.paths.replacement_candidate_next.exists());

    write_bytes_once(
        &fixture.paths.replacement_submission_next,
        &submission_record,
        &fixture.paths.root,
    )
    .unwrap();
    assert_eq!(
        load_replacement_candidate_submission(&fixture.node_data_dir).unwrap(),
        Some(expected_submission)
    );
    assert!(fixture.paths.replacement_submission.exists());
    assert!(!fixture.paths.replacement_submission_next.exists());
}

#[test]
fn restart_never_auto_promotes_and_cleans_an_already_committed_candidate() {
    let fixture = replacement_fixture();
    let candidate_record = std::fs::read(&fixture.paths.replacement_candidate).unwrap();
    let submission_record = std::fs::read(&fixture.paths.replacement_submission).unwrap();
    std::fs::remove_file(&fixture.paths.replacement_submission).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
    write_bytes_once(
        &fixture.paths.replacement_candidate_next,
        &candidate_record,
        &fixture.paths.root,
    )
    .unwrap();
    let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
    reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
    assert!(!fixture.paths.replacement_candidate_next.exists());
    write_bytes_once(
        &fixture.paths.replacement_submission,
        &submission_record,
        &fixture.paths.root,
    )
    .unwrap();

    write_bytes_once(
        &fixture.paths.replacement_promotion,
        &fixture.authorization.encode_canonical(),
        &fixture.paths.root,
    )
    .unwrap();
    let candidate_bytes = fixture.candidate.encode_canonical().unwrap();
    write_bytes_once(
        &fixture.paths.next_manifest,
        &candidate_bytes,
        &fixture.paths.root,
    )
    .unwrap();
    reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.active
    );
    assert!(fixture.paths.next_manifest.exists());
    assert!(fixture.paths.replacement_candidate.exists());
    assert!(fixture.paths.replacement_submission.exists());

    fs::rename(&fixture.paths.next_manifest, &fixture.paths.manifest).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
    reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.candidate
    );
    assert!(!fixture.paths.next_manifest.exists());
    assert!(!fixture.paths.replacement_candidate.exists());
    assert!(!fixture.paths.replacement_submission.exists());
    assert!(fixture.paths.replacement_promotion.exists());
}

#[test]
fn restart_discards_an_uncommitted_torn_replacement_scratch() {
    let fixture = replacement_fixture();
    let scratch = fixture.paths.root.join("replacement-write.tmp");
    write_bytes_once(&scratch, b"torn", &fixture.paths.root).unwrap();

    assert!(
        load_replacement_candidate_submission(&fixture.node_data_dir)
            .unwrap()
            .is_some()
    );
    assert!(!scratch.exists());
    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.active
    );
}

#[test]
fn restart_recovers_submission_only_residue_after_committed_promotion() {
    let fixture = replacement_fixture();
    write_bytes_once(
        &fixture.paths.replacement_promotion,
        &fixture.authorization.encode_canonical(),
        &fixture.paths.root,
    )
    .unwrap();
    write_bytes_once(
        &fixture.paths.next_manifest,
        &fixture.candidate.encode_canonical().unwrap(),
        &fixture.paths.root,
    )
    .unwrap();
    fs::rename(&fixture.paths.next_manifest, &fixture.paths.manifest).unwrap();
    std::fs::remove_file(&fixture.paths.replacement_candidate).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();

    let node_host = NodeHostNoiseKey::load(&fixture.paths.noise_key).unwrap();
    reconcile_replacement_state(&fixture.paths, &node_host).unwrap();
    assert_eq!(
        read_manifest(&fixture.paths.manifest).unwrap(),
        fixture.candidate
    );
    assert!(!fixture.paths.replacement_submission.exists());
    assert!(fixture.paths.replacement_promotion.exists());
}
