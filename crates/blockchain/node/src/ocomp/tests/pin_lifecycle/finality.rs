use super::*;

#[test]
fn finalized_request_is_durable_across_nodes_before_canonical_job_binding() {
    let fixture = production_candidate_source();
    let job_id = fixture_job_id(fixture.candidate);
    let roots = (0..4)
        .map(|_| tempfile::tempdir().unwrap())
        .collect::<Vec<_>>();
    let mut journal_bytes = Vec::new();
    for root in &roots {
        let coordinator = OcompRetentionCoordinator::open(root.path(), fixture.source.clone());
        assert_eq!(coordinator.status(), RetentionStatus::Empty);
        fixture.admit(&coordinator);
        assert_eq!(
            ready_record(&coordinator),
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::AwaitingJobFinalization {
                    candidate: fixture.candidate
                },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        assert!(!coordinator.is_exportable(job_id));
        journal_bytes.push(fs::read(root.path().join("pin.v1")).unwrap());
    }
    assert!(journal_bytes.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn finalized_request_waits_for_canonical_job_across_restart_and_later_frames() {
    let request = block(100, B256::repeat_byte(0x30), 0);
    let candidate = candidate(&request);
    let job_id = fixture_job_id(candidate);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().unwrap();
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    FinalizedFrameDriver::admit(&source, &coordinator, &request).unwrap();
    let before = fs::read(root.path().join("pin.v1")).unwrap();
    let mut pending = source.canonical_job(candidate);
    pending.finalized = None;
    assert!(coordinator
        .bind_canonical_finalized_job(candidate.block_hash, &pending)
        .is_err());
    for height in 101..110 {
        let frame = frame_for_block(&block(height, B256::repeat_byte(0x31), 1), vec![]);
        coordinator.reconcile_finalized_frame(&frame, None).unwrap();
    }
    assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
    drop(coordinator);
    let restarted = OcompRetentionCoordinator::open(root.path(), source.clone());
    FinalizedFrameDriver::admit(&source, &restarted, &request).unwrap();
    assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
    FinalizedFrameDriver::bind(&source, &restarted, &request);
    assert_eq!(
        ready_record(&restarted),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 101,
                open_height: 105,
                deadline_height: 115,
            },
        }
    );
    assert!(restarted.is_exportable(job_id));
    assert!(!restarted.is_signable(job_id));
}

#[test]
fn losing_candidate_never_enters_registry_and_finalized_replacement_is_admitted_once() {
    let parent = block(24, B256::repeat_byte(0x24), 0x24);
    let losing = block_extending(25, B256::repeat_byte(0xa5), parent.block_hash(), 0xa5);
    let winner = block_extending(25, B256::repeat_byte(0xb5), parent.block_hash(), 0xb5);
    let losing_candidate = candidate(&losing);
    let winner_candidate = candidate(&winner);
    // Both execution states exist in the fixture, as after execution/reexecution.
    // Only the canonical finalized reader supplies a frame to retention.
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (losing_candidate, fixture_job_id(losing_candidate)),
        (winner_candidate, fixture_job_id(winner_candidate)),
    ]));
    let root = tempfile::tempdir().unwrap();
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    FinalizedFrameDriver::admit(&source, &coordinator, &parent).unwrap();
    assert_eq!(coordinator.status(), RetentionStatus::Empty);
    assert!(!root.path().join("pin.v1").exists());
    drop(coordinator);
    let restarted = OcompRetentionCoordinator::open(root.path(), source.clone());
    FinalizedFrameDriver::admit(&source, &restarted, &winner).unwrap();
    let before = fs::read(root.path().join("pin.v1")).unwrap();
    FinalizedFrameDriver::admit(&source, &restarted, &winner).unwrap();
    assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
    assert_eq!(
        inspect_retention_journal(root.path()).unwrap().records,
        vec![(
            winner.block_hash(),
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::AwaitingJobFinalization {
                    candidate: winner_candidate
                }
            }
        )]
    );
    assert!(!restarted.is_exportable(fixture_job_id(losing_candidate)));
}

#[test]
fn finalized_replacement_without_request_leaves_no_job_or_retention_pin() {
    let losing = block(25, B256::repeat_byte(0xa5), 0xa5);
    let winner = block(25, B256::repeat_byte(0xb5), 0xb5);
    let losing_candidate = candidate(&losing);
    let source = Arc::new(DeterministicProofSource::with_jobs([(
        losing_candidate,
        fixture_job_id(losing_candidate),
    )]));
    let root = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        FinalizedFrameDriver::admit(&source, &coordinator, &winner).unwrap();
        assert_eq!(coordinator.status(), RetentionStatus::Empty);
        assert!(!root.path().join("pin.v1").exists());
    }
}

#[test]
fn finalized_event_mismatch_cannot_create_a_durable_registration() {
    let fixture = production_candidate_source();
    let observation = observe_finalized_request(&fixture.frame())
        .unwrap()
        .unwrap();
    let mut wrong_intent = observation;
    wrong_intent.intent_id = B256::repeat_byte(0xee);
    let mut wrong_day = observation;
    wrong_day.wwd += 1;
    let mut wrong_nonce = observation;
    wrong_nonce.pending_nonce += 1;
    let mut wrong_attempt = observation;
    wrong_attempt.attempt += 1;
    let mut wrong_preconditions = observation;
    wrong_preconditions.activation_preconditions_hash = B256::repeat_byte(0xee);
    for wrong in [
        wrong_intent,
        wrong_day,
        wrong_nonce,
        wrong_attempt,
        wrong_preconditions,
    ] {
        let event = IMetadosis::OffchainJobRequested {
            intentId: wrong.intent_id,
            wwd: wrong.wwd,
            pendingNonce: wrong.pending_nonce,
            attempt: wrong.attempt,
            activationPreconditionsHash: wrong.activation_preconditions_hash,
        };
        let mut receipts = fixture.receipts.clone();
        receipts[0].logs[0].data = event.encode_log_data();
        let frame = frame_for_block(&fixture.request, receipts);
        let root = tempfile::tempdir().unwrap();
        let coordinator = OcompRetentionCoordinator::open(root.path(), fixture.source.clone());
        let result = coordinator
            .reconcile_finalized_frame(&frame, observe_finalized_request(&frame).unwrap());
        assert!(
            matches!(result, Err(RetentionError::Source(_))),
            "{wrong:?}: {result:?}"
        );
        assert_eq!(coordinator.status(), RetentionStatus::Empty);
        assert!(!root.path().join("pin.v1").exists());
    }
}
