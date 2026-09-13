use super::*;

#[test]
fn unified_finalized_frame_supplies_retention_receipts_without_a_second_read() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let receipts = pending_receipts
        .lock()
        .expect("pending receipt fixture")
        .take()
        .expect("fixture receipts")
        .1;
    let frame = FinalizedFrame::for_test(
        BlockNumHash::new(request.number(), request.block_hash()),
        request.header().inner.parent_hash,
        request.header().inner.state_root,
        Block::default(),
        receipts,
    );

    let observation = observe_finalized_request(&frame)
        .expect("frame observation")
        .expect("request observation");
    assert_eq!(
        source
            .candidate_for_finalized_observation(&frame, observation)
            .expect("frame candidate"),
        candidate
    );
    assert!(
        pending_receipts
            .lock()
            .expect("pending receipt fixture")
            .is_none(),
        "the finalized path must not call the pending/canonical receipt reader"
    );
}

#[test]
fn finalized_request_replay_preserves_every_durable_lifecycle_state_after_restart() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let receipts = pending_receipts.lock().unwrap().take().unwrap().1;
    let frame = FinalizedFrame::for_test(
        BlockNumHash::new(request.number(), request.block_hash()),
        request.header().inner.parent_hash,
        request.header().inner.state_root,
        Block::default(),
        receipts,
    );
    let observation = observe_finalized_request(&frame).unwrap().unwrap();
    let job_id = production_intent(request.number())
        .job_id(
            candidate.block_hash,
            candidate.state_root,
            &poc_schema_limits(),
        )
        .unwrap();
    let finality_recorded_height = candidate.block_number + 1;
    let open_height = candidate.block_number + 5;
    let deadline_height = candidate.block_number + 15;
    let export = ExportAuthorityV1 {
        source_generation: 2,
        lease_generation: 3,
        manifest_hash: B256::repeat_byte(0x59),
    };
    let states = [
        PinStateV1::Tentative { candidate },
        PinStateV1::Finalized {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        },
        PinStateV1::Exported {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            export,
        },
        PinStateV1::Terminal {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation: 2,
            export: None,
            terminal_height: deadline_height,
            release_height: deadline_height + 64,
        },
        PinStateV1::GcPending {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation: 2,
            export: None,
            terminal_height: deadline_height,
            release_height: deadline_height + 64,
        },
        PinStateV1::Released {
            candidate,
            job_id: Some(job_id),
            source_generation: Some(2),
            reason: PinReleaseReason::RetentionSatisfied,
            observed_height: deadline_height + 64,
            export: None,
        },
        PinStateV1::Released {
            candidate,
            job_id: Some(job_id),
            source_generation: Some(2),
            reason: PinReleaseReason::RetentionSatisfied,
            observed_height: deadline_height + 64,
            export: Some(export),
        },
    ];
    for state in states {
        let root = tempfile::tempdir().unwrap();
        let record = PinRecordV1 {
            generation: 8,
            state,
        };
        seed_retention_journal_for_test(
            root.path(),
            8,
            candidate.block_hash,
            vec![(candidate.block_hash, record)],
        )
        .unwrap();
        let before = fs::read(root.path().join("pin.v1")).unwrap();
        for _ in 0..2 {
            let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
            coordinator
                .reconcile_finalized_frame(&frame, Some(observation))
                .unwrap_or_else(|error| panic!("replay of {state:?} failed: {error}"));
            assert_eq!(ready_record(&coordinator), record);
            assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
        }
    }
}

#[test]
fn finalized_request_replay_rejects_conflicts_and_orphaned_authority_without_mutation() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let frame = FinalizedFrame::for_test(
        BlockNumHash::new(request.number(), request.block_hash()),
        request.header().inner.parent_hash,
        request.header().inner.state_root,
        Block::default(),
        pending_receipts.lock().unwrap().take().unwrap().1,
    );
    let observation = observe_finalized_request(&frame).unwrap().unwrap();
    let mut conflicting = candidate;
    conflicting.state_root = B256::repeat_byte(0x91);
    let states = [
        PinStateV1::Tentative {
            candidate: conflicting,
        },
        PinStateV1::OrphanGcPending {
            candidate,
            observed_height: request.number() + 1,
        },
        PinStateV1::Released {
            candidate,
            job_id: None,
            source_generation: None,
            reason: PinReleaseReason::Orphaned,
            observed_height: request.number() + 1,
            export: None,
        },
    ];
    for state in states {
        let root = tempfile::tempdir().unwrap();
        seed_retention_journal_for_test(
            root.path(),
            8,
            candidate.block_hash,
            vec![(
                candidate.block_hash,
                PinRecordV1 {
                    generation: 8,
                    state,
                },
            )],
        )
        .unwrap();
        let before = fs::read(root.path().join("pin.v1")).unwrap();
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        let error = coordinator
            .reconcile_finalized_frame(&frame, Some(observation))
            .unwrap_err();
        assert!(matches!(
            error,
            RetentionError::ConflictingCandidate | RetentionError::OrphanedCandidate
        ));
        assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
    }
}

#[test]
fn unified_retention_waits_for_and_copies_canonical_finalized_job() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let receipts = pending_receipts
        .lock()
        .expect("pending receipt fixture")
        .take()
        .expect("fixture receipts")
        .1;
    let frame = FinalizedFrame::for_test(
        BlockNumHash::new(request.number(), request.block_hash()),
        request.header().inner.parent_hash,
        request.header().inner.state_root,
        Block::default(),
        receipts,
    );
    let observation = observe_finalized_request(&frame)
        .expect("frame observation")
        .expect("request observation");
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);

    coordinator
        .reconcile_finalized_frame(&frame, Some(observation))
        .expect("request frame reconciliation");
    assert!(matches!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate: actual },
        } if actual == candidate
    ));

    let intent = production_intent(request.number());
    let limits = poc_schema_limits();
    let job_id = intent
        .job_id(candidate.block_hash, candidate.state_root, &limits)
        .expect("fixture JobId");
    let canonical = OcompJobRecordV1 {
        intent,
        intent_height: candidate.block_number,
        status: OcompJobStatus::AwaitingFinality,
        finalized: Some(OcompFinalizedJobV1 {
            job_id,
            finalized_request_block_hash: candidate.block_hash,
            finalized_request_state_root: candidate.state_root,
            finality_recorded_height: candidate.block_number + 1,
            open_height: candidate.block_number + 5,
            deadline_height: candidate.block_number + 15,
            quorum: None,
        }),
        terminal: None,
    };

    let ack = coordinator
        .bind_canonical_finalized_job(candidate.block_hash, &canonical)
        .expect("canonical finalized job binding");
    assert_eq!(ack.generation, 2);
    assert_eq!(
        coordinator
            .finalized_job_record(job_id)
            .expect("finalized retention record"),
        (
            2,
            FinalizedJobPinV1 {
                candidate,
                job_id,
                finality_recorded_height: candidate.block_number + 1,
                open_height: candidate.block_number + 5,
                deadline_height: candidate.block_number + 15,
            }
        )
    );
}

#[test]
fn canonical_terminal_binding_closes_without_waiting_for_another_finalized_block() {
    let fixture = production_candidate_source();
    let root = tempfile::tempdir().unwrap();
    let canonical = canonical_terminal_fixture(fixture.candidate, OcompJobStatus::Expired);
    let coordinator = OcompRetentionCoordinator::open(root.path(), fixture.source.clone());
    coordinator.record_tentative(fixture.candidate).unwrap();
    coordinator
        .bind_canonical_finalized_job(fixture.candidate.block_hash, &canonical)
        .unwrap();
    let target = canonical.finalized.as_ref().unwrap().deadline_height + 70;
    coordinator
        .reconcile_canonical_terminal(&canonical, target)
        .unwrap();
    let terminal = ready_record(&coordinator);
    assert!(matches!(
        terminal.state,
        PinStateV1::Terminal { export: None, .. }
    ));
    drop(coordinator);
    let reopened = OcompRetentionCoordinator::open(root.path(), fixture.source);
    reopened
        .reconcile_canonical_terminal(&canonical, target)
        .unwrap();
    assert_eq!(ready_record(&reopened), terminal);
    assert!(
        matches!(terminal.state, PinStateV1::Terminal { release_height, .. } if release_height <= target),
        "historical retirement must not restart its 64-block clock at the replay target"
    );
    reopened
        .release_due(target)
        .unwrap()
        .expect("historical lease is immediately due at the same target");
    assert!(matches!(
        ready_record(&reopened).state,
        PinStateV1::Released { export: None, .. }
    ));
}

#[test]
fn late_canonical_ack_recovers_metadata_without_reactivating_terminal_gc_or_released_jobs() {
    for status in [OcompJobStatus::Completed, OcompJobStatus::Failed] {
        let fixture = production_candidate_source();
        let canonical = canonical_terminal_fixture(fixture.candidate, status);
        let finalized = canonical.finalized.as_ref().unwrap();
        let export = ExportAuthorityV1 {
            source_generation: 2,
            lease_generation: 3,
            manifest_hash: B256::repeat_byte(0xaa),
        };
        for phase in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let state = match phase {
                0 => PinStateV1::Terminal {
                    candidate: fixture.candidate,
                    job_id: finalized.job_id,
                    finality_recorded_height: finalized.finality_recorded_height,
                    open_height: finalized.open_height,
                    deadline_height: finalized.deadline_height,
                    source_generation: 2,
                    export: None,
                    terminal_height: finalized.deadline_height,
                    release_height: finalized.deadline_height + 64,
                },
                1 => PinStateV1::GcPending {
                    candidate: fixture.candidate,
                    job_id: finalized.job_id,
                    finality_recorded_height: finalized.finality_recorded_height,
                    open_height: finalized.open_height,
                    deadline_height: finalized.deadline_height,
                    source_generation: 2,
                    export: None,
                    terminal_height: finalized.deadline_height,
                    release_height: finalized.deadline_height + 64,
                },
                _ => PinStateV1::Released {
                    candidate: fixture.candidate,
                    job_id: Some(finalized.job_id),
                    source_generation: Some(2),
                    reason: PinReleaseReason::RetentionSatisfied,
                    observed_height: finalized.deadline_height + 64,
                    export: None,
                },
            };
            seed_retention_journal_for_test(
                root.path(),
                8,
                fixture.candidate.block_hash,
                vec![(
                    fixture.candidate.block_hash,
                    PinRecordV1 {
                        generation: 8,
                        state,
                    },
                )],
            )
            .unwrap();
            let coordinator = OcompRetentionCoordinator::open(root.path(), fixture.source.clone());
            let before = fs::read(root.path().join("pin.v1")).unwrap();
            for invalid in [
                ExportAuthorityV1 {
                    source_generation: 7,
                    ..export
                },
                ExportAuthorityV1 {
                    lease_generation: 0,
                    ..export
                },
                ExportAuthorityV1 {
                    manifest_hash: B256::ZERO,
                    ..export
                },
            ] {
                assert!(coordinator
                    .confirm_canonical_export_ack(&canonical, invalid)
                    .is_err());
                assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
            }
            let expired = canonical_terminal_fixture(fixture.candidate, OcompJobStatus::Expired);
            assert!(coordinator
                .confirm_canonical_export_ack(&expired, export)
                .is_err());
            assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
            coordinator
                .confirm_canonical_export_ack(&canonical, export)
                .unwrap();
            let recovered = ready_record(&coordinator);
            let mut expected = state;
            match &mut expected {
                PinStateV1::Terminal { export: slot, .. }
                | PinStateV1::GcPending { export: slot, .. }
                | PinStateV1::Released { export: slot, .. } => *slot = Some(export),
                _ => unreachable!(),
            }
            assert_eq!(recovered.state, expected);
            assert!(coordinator
                .confirm_canonical_export_ack(
                    &canonical,
                    ExportAuthorityV1 {
                        manifest_hash: B256::repeat_byte(0xbb),
                        ..export
                    }
                )
                .is_err());
            drop(coordinator);
            let reopened = OcompRetentionCoordinator::open(root.path(), fixture.source.clone());
            reopened
                .confirm_canonical_export_ack(&canonical, export)
                .unwrap();
            assert_eq!(ready_record(&reopened), recovered);
        }
    }
}

#[test]
fn canonical_finalized_job_mismatch_cannot_mutate_tentative_retention() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let receipts = pending_receipts
        .lock()
        .expect("pending receipt fixture")
        .take()
        .expect("fixture receipts")
        .1;
    let frame = FinalizedFrame::for_test(
        BlockNumHash::new(request.number(), request.block_hash()),
        request.header().inner.parent_hash,
        request.header().inner.state_root,
        Block::default(),
        receipts,
    );
    let observation = observe_finalized_request(&frame)
        .expect("frame observation")
        .expect("request observation");
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);
    coordinator
        .reconcile_finalized_frame(&frame, Some(observation))
        .expect("request frame reconciliation");

    let intent = production_intent(request.number());
    let limits = poc_schema_limits();
    let job_id = intent
        .job_id(candidate.block_hash, candidate.state_root, &limits)
        .expect("fixture JobId");
    let mismatched = OcompJobRecordV1 {
        intent,
        intent_height: candidate.block_number + 1,
        status: OcompJobStatus::AwaitingFinality,
        finalized: Some(OcompFinalizedJobV1 {
            job_id,
            finalized_request_block_hash: candidate.block_hash,
            finalized_request_state_root: candidate.state_root,
            finality_recorded_height: candidate.block_number + 1,
            open_height: candidate.block_number + 5,
            deadline_height: candidate.block_number + 15,
            quorum: None,
        }),
        terminal: None,
    };

    assert!(matches!(
        coordinator.bind_canonical_finalized_job(candidate.block_hash, &mismatched),
        Err(RetentionError::InvalidTransition(
            "canonical finalized job does not match retained request candidate"
        ))
    ));
    assert!(matches!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate: actual },
        } if actual == candidate
    ));
}
