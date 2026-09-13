use super::*;

fn finalized_join_anchor(fixture: &ReplacementFixture) -> FinalizedJoinAdmissionAnchorV1 {
    let submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap();
    let intent = match evidence {
        AttestationEvidenceV1::Dcap(value) => value.intent,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent,
    };
    FinalizedJoinAdmissionAnchorV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        intent_hash: intent.intent_hash().unwrap(),
        finalized_height: 91,
        finalized_hash: B256::repeat_byte(0x91),
        finalized_state_root: B256::repeat_byte(0x92),
        finalized_consensus_timestamp: 19_000,
    }
}

#[test]
fn committed_join_submission_round_trips_exact_registration_material() {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::GramineDirectDev,
    );
    let candidate_submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence =
        AttestationEvidenceV1::decode_canonical(candidate_submission.evidence()).unwrap();
    let node_signature = *candidate_submission.node_signature();
    let enclave_signature = *candidate_submission.enclave_signature();
    let registration_caller = Address::repeat_byte(0x42);
    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();

    let durable = persist_committed_join_submission(
        &fixture.node_data_dir,
        registration_caller,
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    assert_eq!(durable.registration_caller(), registration_caller);
    assert_eq!(
        load_committed_join_submission(&fixture.node_data_dir)
            .unwrap()
            .unwrap(),
        durable
    );
    assert_eq!(
        persist_committed_join_submission(
            &fixture.node_data_dir,
            registration_caller,
            &evidence,
            &node_signature,
            &enclave_signature,
        )
        .unwrap(),
        durable
    );
    assert!(persist_committed_join_submission(
        &fixture.node_data_dir,
        Address::repeat_byte(0x43),
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap_err()
    .to_string()
    .contains("conflicts"));
}

#[test]
fn committed_join_relay_round_trips_exact_raw_transaction_and_scan_origin() {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::GramineDirectDev,
    );
    let candidate_submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence =
        AttestationEvidenceV1::decode_canonical(candidate_submission.evidence()).unwrap();
    let node_signature = *candidate_submission.node_signature();
    let enclave_signature = *candidate_submission.enclave_signature();
    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();
    persist_committed_join_submission(
        &fixture.node_data_dir,
        Address::repeat_byte(0x42),
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();

    let raw_transaction = [0x91, 0x92, 0x93];
    let relay = persist_committed_join_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x90),
        77,
        &raw_transaction,
    )
    .unwrap();
    assert_eq!(relay.transaction_hash(), keccak256(raw_transaction));
    assert_eq!(relay.from_block(), 77);
    assert_eq!(relay.raw_transaction(), raw_transaction);
    assert_eq!(
        load_committed_join_relay(&fixture.node_data_dir)
            .unwrap()
            .unwrap(),
        relay
    );
    assert_eq!(
        persist_committed_join_relay(
            &fixture.node_data_dir,
            B256::repeat_byte(0x90),
            77,
            &raw_transaction,
        )
        .unwrap(),
        relay
    );
    assert!(persist_committed_join_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x90),
        78,
        &raw_transaction,
    )
    .unwrap_err()
    .to_string()
    .contains("conflicts"));
    assert_eq!(
        std::fs::metadata(&fixture.paths.committed_join_submission)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&fixture.paths.committed_join_relay)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn committed_join_restart_recovers_only_fsynced_next_checkpoints() {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::GramineDirectDev,
    );
    let candidate_submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence =
        AttestationEvidenceV1::decode_canonical(candidate_submission.evidence()).unwrap();
    let node_signature = *candidate_submission.node_signature();
    let enclave_signature = *candidate_submission.enclave_signature();
    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();
    let submission = persist_committed_join_submission(
        &fixture.node_data_dir,
        Address::repeat_byte(0x42),
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    let submission_bytes = std::fs::read(&fixture.paths.committed_join_submission).unwrap();
    std::fs::remove_file(&fixture.paths.committed_join_submission).unwrap();
    write_bytes_once(
        &fixture.paths.committed_join_submission_next,
        &submission_bytes,
        &fixture.paths.root,
    )
    .unwrap();
    assert_eq!(
        load_committed_join_submission(&fixture.node_data_dir)
            .unwrap()
            .unwrap(),
        submission
    );

    let relay = persist_committed_join_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x81),
        82,
        &[0x83, 0x84],
    )
    .unwrap();
    let relay_bytes = std::fs::read(&fixture.paths.committed_join_relay).unwrap();
    std::fs::remove_file(&fixture.paths.committed_join_relay).unwrap();
    write_bytes_once(
        &fixture.paths.committed_join_relay_next,
        &relay_bytes,
        &fixture.paths.root,
    )
    .unwrap();
    assert_eq!(
        load_committed_join_relay(&fixture.node_data_dir)
            .unwrap()
            .unwrap(),
        relay
    );
    assert!(!fixture.paths.committed_join_submission_next.exists());
    assert!(!fixture.paths.committed_join_relay_next.exists());
}

#[test]
fn committed_join_cleanup_requires_the_exact_intent_and_is_idempotent() {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::GramineDirectDev,
    );
    let candidate_submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence =
        AttestationEvidenceV1::decode_canonical(candidate_submission.evidence()).unwrap();
    let intent_hash = match &evidence {
        AttestationEvidenceV1::Dcap(value) => value.intent.intent_hash().unwrap(),
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent.intent_hash().unwrap(),
    };
    let node_signature = *candidate_submission.node_signature();
    let enclave_signature = *candidate_submission.enclave_signature();
    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();
    persist_committed_join_submission(
        &fixture.node_data_dir,
        Address::repeat_byte(0x42),
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    persist_committed_join_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x81),
        82,
        &[0x83, 0x84],
    )
    .unwrap();

    assert!(
        clear_committed_join_checkpoint(&fixture.node_data_dir, B256::repeat_byte(0x85),)
            .unwrap_err()
            .to_string()
            .contains("another intent")
    );
    assert!(fixture.paths.committed_join_submission.exists());
    assert!(fixture.paths.committed_join_relay.exists());

    std::fs::remove_file(&fixture.paths.committed_join_relay).unwrap();
    File::open(&fixture.paths.root).unwrap().sync_all().unwrap();
    clear_committed_join_checkpoint(&fixture.node_data_dir, intent_hash).unwrap();
    assert!(!fixture.paths.committed_join_submission.exists());
    assert!(!fixture.paths.committed_join_relay.exists());
    clear_committed_join_checkpoint(&fixture.node_data_dir, intent_hash).unwrap();
}

#[test]
fn corrupt_committed_join_checkpoint_is_retained_and_rejected() {
    let fixture = replacement_fixture_for_mode(
        AttestationOperationV1::RegisterEnclave,
        AttestationMode::GramineDirectDev,
    );
    let candidate_submission = load_replacement_candidate_submission(&fixture.node_data_dir)
        .unwrap()
        .unwrap();
    let evidence =
        AttestationEvidenceV1::decode_canonical(candidate_submission.evidence()).unwrap();
    let node_signature = *candidate_submission.node_signature();
    let enclave_signature = *candidate_submission.enclave_signature();
    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();
    persist_committed_join_submission(
        &fixture.node_data_dir,
        Address::repeat_byte(0x42),
        &evidence,
        &node_signature,
        &enclave_signature,
    )
    .unwrap();
    std::fs::write(&fixture.paths.committed_join_submission, [0xff, 0x00]).unwrap();

    assert!(load_committed_join_submission(&fixture.node_data_dir).is_err());
    assert!(fixture.paths.committed_join_submission.exists());
}

#[test]
fn finalized_join_anchor_is_owner_only_and_round_trips_exactly() {
    let fixture = replacement_fixture();
    let anchor = finalized_join_anchor(&fixture);

    assert_eq!(
        persist_finalized_join_admission_anchor(&fixture.node_data_dir, anchor).unwrap(),
        anchor
    );
    assert_eq!(
        load_finalized_join_admission_anchor(&fixture.node_data_dir).unwrap(),
        Some(anchor)
    );
    assert_eq!(
        std::fs::metadata(&fixture.paths.finalized_join_admission_anchor)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn finalized_join_anchor_replay_is_exact_and_replacement_is_strictly_monotonic() {
    let fixture = replacement_fixture();
    let anchor = finalized_join_anchor(&fixture);
    persist_finalized_join_admission_anchor(&fixture.node_data_dir, anchor).unwrap();
    persist_finalized_join_admission_anchor(&fixture.node_data_dir, anchor).unwrap();

    let conflicting = FinalizedJoinAdmissionAnchorV1 {
        finalized_hash: B256::repeat_byte(0xa1),
        ..anchor
    };
    assert!(
        persist_finalized_join_admission_anchor(&fixture.node_data_dir, conflicting)
            .unwrap_err()
            .to_string()
            .contains("conflicts")
    );

    let lower = FinalizedJoinAdmissionAnchorV1 {
        finalized_height: anchor.finalized_height - 1,
        finalized_hash: B256::repeat_byte(0xa2),
        ..anchor
    };
    assert!(
        persist_finalized_join_admission_anchor(&fixture.node_data_dir, lower)
            .unwrap_err()
            .to_string()
            .contains("newer")
    );

    let later = FinalizedJoinAdmissionAnchorV1 {
        finalized_height: anchor.finalized_height + 1,
        finalized_hash: B256::repeat_byte(0xa3),
        finalized_state_root: B256::repeat_byte(0xa4),
        finalized_consensus_timestamp: anchor.finalized_consensus_timestamp + 1,
        ..anchor
    };
    persist_finalized_join_admission_anchor(&fixture.node_data_dir, later).unwrap();
    assert_eq!(
        load_finalized_join_admission_anchor(&fixture.node_data_dir).unwrap(),
        Some(later)
    );
}

#[test]
fn unfinished_join_without_matching_anchor_fails_closed() {
    let fixture = replacement_fixture();
    assert!(load_finalized_join_admission_anchor(&fixture.node_data_dir)
        .unwrap_err()
        .to_string()
        .contains("unfinished join"));

    let mut wrong = finalized_join_anchor(&fixture);
    wrong.intent_hash = B256::repeat_byte(0xb1);
    assert!(
        persist_finalized_join_admission_anchor(&fixture.node_data_dir, wrong)
            .unwrap_err()
            .to_string()
            .contains("durable join intent")
    );
}

#[test]
fn finalized_join_anchor_restart_recovers_only_complete_next_record() {
    let fixture = replacement_fixture();
    let anchor = finalized_join_anchor(&fixture);
    let bytes = anchor.encode_canonical();
    write_bytes_once(
        &fixture.paths.finalized_join_admission_anchor_next,
        &bytes,
        &fixture.paths.root,
    )
    .unwrap();
    write_bytes_once(
        &fixture.paths.finalized_join_admission_anchor_scratch,
        b"torn",
        &fixture.paths.root,
    )
    .unwrap();

    assert_eq!(
        load_finalized_join_admission_anchor(&fixture.node_data_dir).unwrap(),
        Some(anchor)
    );
    assert!(fixture.paths.finalized_join_admission_anchor.exists());
    assert!(!fixture.paths.finalized_join_admission_anchor_next.exists());
    assert!(!fixture
        .paths
        .finalized_join_admission_anchor_scratch
        .exists());
}

#[test]
fn corrupt_finalized_join_anchor_is_retained_and_rejected() {
    let fixture = replacement_fixture();
    let anchor = finalized_join_anchor(&fixture);
    persist_finalized_join_admission_anchor(&fixture.node_data_dir, anchor).unwrap();
    std::fs::write(&fixture.paths.finalized_join_admission_anchor, [0xff, 0x00]).unwrap();

    assert!(load_finalized_join_admission_anchor(&fixture.node_data_dir).is_err());
    assert!(fixture.paths.finalized_join_admission_anchor.exists());
}
