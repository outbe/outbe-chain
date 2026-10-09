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

/// Promote the DirectDev candidate, then persist its committed-join
/// submission from the registration caller 0x42.
fn promote_and_persist_join_submission(
    fixture: &ReplacementFixture,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> CommittedJoinSubmissionV1 {
    promote_replacement_candidate(&fixture.node_data_dir, &fixture.authorization).unwrap();
    persist_committed_join_submission(
        &fixture.node_data_dir,
        Address::repeat_byte(0x42),
        evidence,
        node_signature,
        enclave_signature,
    )
    .unwrap()
}

#[test]
fn committed_join_submission_round_trips_exact_registration_material() {
    let DirectDevRegistration {
        fixture,
        evidence,
        node_signature,
        enclave_signature,
    } = direct_dev_registration();
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
    let mut expected_bytes = vec![1];
    expected_bytes.extend_from_slice(registration_caller.as_slice());
    expected_bytes.extend_from_slice(
        &u32::try_from(durable.evidence().len())
            .unwrap()
            .to_be_bytes(),
    );
    expected_bytes.extend_from_slice(durable.evidence());
    expected_bytes.extend_from_slice(durable.node_signature());
    expected_bytes.extend_from_slice(durable.enclave_signature());
    assert_eq!(
        std::fs::read(&fixture.paths.committed_join_submission).unwrap(),
        expected_bytes
    );
    assert_eq!(
        durable.submission_hash().unwrap(),
        keccak256(&expected_bytes)
    );
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
    let DirectDevRegistration {
        fixture,
        evidence,
        node_signature,
        enclave_signature,
    } = direct_dev_registration();
    promote_and_persist_join_submission(&fixture, &evidence, &node_signature, &enclave_signature);

    let raw_transaction = [0x91, 0x92, 0x93];
    let relay = persist_committed_join_relay(
        &fixture.node_data_dir,
        B256::repeat_byte(0x90),
        77,
        &raw_transaction,
    )
    .unwrap();
    let submission_bytes = std::fs::read(&fixture.paths.committed_join_submission).unwrap();
    let mut expected_bytes = vec![1];
    expected_bytes.extend_from_slice(keccak256(submission_bytes).as_slice());
    expected_bytes.extend_from_slice(B256::repeat_byte(0x90).as_slice());
    expected_bytes.extend_from_slice(keccak256(raw_transaction).as_slice());
    expected_bytes.extend_from_slice(&77_u64.to_be_bytes());
    expected_bytes.extend_from_slice(&u32::try_from(raw_transaction.len()).unwrap().to_be_bytes());
    expected_bytes.extend_from_slice(&raw_transaction);
    assert_eq!(
        std::fs::read(&fixture.paths.committed_join_relay).unwrap(),
        expected_bytes
    );
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
    let DirectDevRegistration {
        fixture,
        evidence,
        node_signature,
        enclave_signature,
    } = direct_dev_registration();
    let submission = promote_and_persist_join_submission(
        &fixture,
        &evidence,
        &node_signature,
        &enclave_signature,
    );
    stage_existing_record_as_next(
        &fixture.paths.committed_join_submission,
        &fixture.paths.committed_join_submission_next,
        &fixture.paths.root,
        DirectorySync::Skip,
    );
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
    stage_existing_record_as_next(
        &fixture.paths.committed_join_relay,
        &fixture.paths.committed_join_relay_next,
        &fixture.paths.root,
        DirectorySync::Skip,
    );
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
    let DirectDevRegistration {
        fixture,
        evidence,
        node_signature,
        enclave_signature,
    } = direct_dev_registration();
    let intent_hash = match &evidence {
        AttestationEvidenceV1::Dcap(value) => value.intent.intent_hash().unwrap(),
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent.intent_hash().unwrap(),
    };
    promote_and_persist_join_submission(&fixture, &evidence, &node_signature, &enclave_signature);
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
    let DirectDevRegistration {
        fixture,
        evidence,
        node_signature,
        enclave_signature,
    } = direct_dev_registration();
    promote_and_persist_join_submission(&fixture, &evidence, &node_signature, &enclave_signature);
    std::fs::write(&fixture.paths.committed_join_submission, [0xff, 0x00]).unwrap();

    assert!(load_committed_join_submission(&fixture.node_data_dir).is_err());
    assert!(fixture.paths.committed_join_submission.exists());
}

struct EncodedCommittedJoinFixture {
    fixture: ReplacementFixture,
    submission: Vec<u8>,
    relay: Vec<u8>,
}

fn encoded_committed_join_fixture() -> Result<EncodedCommittedJoinFixture, TransportError> {
    let fixture = replacement_fixture_for_operation(AttestationOperationV1::RegisterEnclave);
    let replacement = std::fs::read(&fixture.paths.replacement_submission)?;
    let mut submission = vec![1];
    submission.extend_from_slice(Address::repeat_byte(0x42).as_slice());
    submission.extend_from_slice(&replacement[1..]);
    write_bytes_once(
        &fixture.paths.committed_join_submission,
        &submission,
        &fixture.paths.root,
    )?;
    read_committed_join_submission(&fixture.paths.committed_join_submission)?;

    let raw_transaction = [0x91, 0x92];
    let mut relay = vec![1];
    relay.extend_from_slice(keccak256(&submission).as_slice());
    relay.extend_from_slice(B256::repeat_byte(0x90).as_slice());
    relay.extend_from_slice(keccak256(raw_transaction).as_slice());
    relay.extend_from_slice(&77_u64.to_be_bytes());
    relay.extend_from_slice(&2_u32.to_be_bytes());
    relay.extend_from_slice(&raw_transaction);
    write_bytes_once(
        &fixture.paths.committed_join_relay,
        &relay,
        &fixture.paths.root,
    )?;
    read_committed_join_relay(&fixture.paths.committed_join_relay)?;
    Ok(EncodedCommittedJoinFixture {
        fixture,
        submission,
        relay,
    })
}

#[test]
fn committed_join_submission_decode_preserves_first_error() {
    let EncodedCommittedJoinFixture {
        fixture,
        submission,
        ..
    } = encoded_committed_join_fixture().unwrap();

    let mut invalid_frame = submission.clone();
    invalid_frame[0] = 2;
    invalid_frame[21..25].fill(0);
    std::fs::write(&fixture.paths.committed_join_submission, invalid_frame).unwrap();
    assert_eq!(
        read_committed_join_submission(&fixture.paths.committed_join_submission)
            .unwrap_err()
            .to_string(),
        "codec error: committed join submission framing is invalid"
    );

    let mut invalid_length = submission.clone();
    invalid_length[1..21].fill(0);
    invalid_length[21..25].fill(0);
    std::fs::write(&fixture.paths.committed_join_submission, invalid_length).unwrap();
    assert_eq!(
        read_committed_join_submission(&fixture.paths.committed_join_submission)
            .unwrap_err()
            .to_string(),
        "codec error: committed join evidence length is non-canonical"
    );

    let mut zero_caller = submission;
    zero_caller[1..21].fill(0);
    zero_caller[25] = 0xff;
    std::fs::write(&fixture.paths.committed_join_submission, zero_caller).unwrap();
    assert_eq!(
        read_committed_join_submission(&fixture.paths.committed_join_submission)
            .unwrap_err()
            .to_string(),
        "codec error: committed join registration caller is zero"
    );
}

#[test]
fn committed_join_relay_decode_preserves_first_error() {
    let EncodedCommittedJoinFixture { fixture, relay, .. } =
        encoded_committed_join_fixture().unwrap();
    super::fixtures::assert_relay_decode_precedence(
        &fixture.paths,
        &relay,
        super::fixtures::RelayRecordKind::CommittedJoin,
    )
    .unwrap();
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
