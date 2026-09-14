use super::*;

#[test]
fn durable_registry_tracks_more_than_256_independent_jobs_across_restart() {
    let requests = (0_u64..257)
        .map(|ordinal| {
            block(
                1_000 + ordinal,
                keccak256(ordinal.to_be_bytes()),
                u8::try_from(ordinal & 0xff).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let source = Arc::new(DeterministicProofSource::with_jobs(requests.iter().map(
        |request| {
            let candidate = candidate(request);
            (candidate, fixture_job_id(candidate))
        },
    )));
    let root = tempfile::tempdir().expect("multi-job journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    for request in &requests {
        FinalizedFrameDriver::admit(&source, &coordinator, request)
            .expect("journal wire format, not an OCOMP product cap, bounds records");
    }
    drop(coordinator);

    let snapshot = inspect_retention_journal(root.path()).expect("inspect durable multi-job state");
    assert_eq!(snapshot.records.len(), 257);
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(restarted.status(), RetentionStatus::Ready(_)));
    assert_eq!(
        inspect_retention_journal(root.path())
            .unwrap()
            .records
            .len(),
        257
    );
}

#[test]
fn ocm_pin_001_pressure_watermark_compacts_only_released_records_and_survives_restart() {
    fn pressure_candidate(ordinal: u64) -> CandidatePinV1 {
        candidate(&block(ordinal + 1, keccak256(ordinal.to_be_bytes()), 1))
    }

    let root = tempfile::tempdir().expect("pressure journal root");
    let watermark = retention_pressure_watermark_for_test();
    let old_awaiting_job = pressure_candidate(1);
    let finalized = pressure_candidate(2);
    let due_terminal = pressure_candidate(3);
    let future_terminal = pressure_candidate(4);
    let due_job = keccak256(b"pressure due job");
    let future_job = keccak256(b"pressure future job");
    let mut records = vec![
        (
            old_awaiting_job.block_hash,
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::AwaitingJobFinalization {
                    candidate: old_awaiting_job,
                },
            },
        ),
        (
            finalized.block_hash,
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::Finalized {
                    candidate: finalized,
                    job_id: keccak256(b"pressure finalized job"),
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 180,
                },
            },
        ),
        (
            due_terminal.block_hash,
            PinRecordV1 {
                generation: 3,
                state: PinStateV1::Terminal {
                    candidate: due_terminal,
                    job_id: due_job,
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 180,
                    source_generation: 1,
                    export: Some(ExportAuthorityV1 {
                        source_generation: 1,
                        lease_generation: 1,
                        manifest_hash: B256::repeat_byte(0x71),
                    }),
                    terminal_height: 200,
                    release_height: 264,
                },
            },
        ),
        (
            future_terminal.block_hash,
            PinRecordV1 {
                generation: 4,
                state: PinStateV1::Terminal {
                    candidate: future_terminal,
                    job_id: future_job,
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 180,
                    source_generation: 2,
                    export: Some(ExportAuthorityV1 {
                        source_generation: 2,
                        lease_generation: 1,
                        manifest_hash: B256::repeat_byte(0x72),
                    }),
                    terminal_height: 400,
                    release_height: 464,
                },
            },
        ),
    ];
    for ordinal in 4_u64..u64::try_from(watermark).expect("watermark fits u64") {
        let candidate = pressure_candidate(ordinal.saturating_add(10_000));
        records.push((
            candidate.block_hash,
            PinRecordV1 {
                generation: ordinal.saturating_add(1),
                state: PinStateV1::Released {
                    candidate,
                    job_id: keccak256(ordinal.to_be_bytes()),
                    source_generation: ordinal.saturating_add(1),
                    observed_height: ordinal,
                    export: (ordinal % 2 == 0).then_some(ExportAuthorityV1 {
                        source_generation: ordinal.saturating_add(1),
                        lease_generation: 1,
                        manifest_hash: keccak256(ordinal.to_be_bytes()),
                    }),
                },
            },
        ));
    }
    assert_eq!(records.len(), watermark);
    let last_updated = records.last().expect("seed has records").0;
    seed_retention_journal_for_test(
        root.path(),
        u64::try_from(watermark).expect("watermark fits generation"),
        last_updated,
        records,
    )
    .expect("seed canonical pressure journal");

    let new_candidate = pressure_candidate(99_999);
    let new_request = block(100_000, keccak256(99_999_u64.to_be_bytes()), 1);
    let source = Arc::new(DeterministicProofSource::with_jobs([(
        new_candidate,
        fixture_job_id(new_candidate),
    )]));
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    coordinator.set_closure_checkpoint_for_test(u64::MAX);
    assert_eq!(
        inspect_retention_journal(root.path())
            .unwrap()
            .records
            .len(),
        watermark
    );
    coordinator
        .release_due(300)
        .expect("release due terminal under pressure")
        .expect("one due terminal transitions");
    drop(coordinator);
    let compaction_durability = Arc::new(FailOnceDurability::disarmed(FailSync::File));
    let interrupted_compaction = OcompRetentionCoordinator::open_with_durability(
        root.path(),
        source.clone(),
        compaction_durability.clone(),
    );
    compaction_durability.arm();
    interrupted_compaction.set_closure_checkpoint_for_test(u64::MAX);
    assert!(FinalizedFrameDriver::admit(&source, &interrupted_compaction, &new_request).is_err());
    assert!(root.path().join("pin.v1.tmp").is_file());
    drop(interrupted_compaction);
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    coordinator.set_closure_checkpoint_for_test(u64::MAX);
    assert!(matches!(coordinator.status(), RetentionStatus::Ready(_)));
    FinalizedFrameDriver::admit(&source, &coordinator, &new_request)
        .expect("recovered compacted admission is idempotent");

    let snapshot = inspect_retention_journal(root.path()).expect("inspect compacted journal");
    let survivors = snapshot.records.into_iter().collect::<BTreeMap<_, _>>();
    assert_eq!(survivors.len(), 4);
    assert_eq!(
        survivors.get(&old_awaiting_job.block_hash).unwrap().state,
        PinStateV1::AwaitingJobFinalization {
            candidate: old_awaiting_job
        }
    );
    assert!(matches!(
        survivors.get(&finalized.block_hash).unwrap().state,
        PinStateV1::Finalized { candidate, .. } if candidate == finalized
    ));
    assert!(!survivors.contains_key(&due_terminal.block_hash));
    assert!(matches!(
        survivors.get(&future_terminal.block_hash).unwrap().state,
        PinStateV1::Terminal {
            candidate,
            job_id,
            release_height: 464,
            ..
        } if candidate == future_terminal && job_id == future_job
    ));
    assert_eq!(
        survivors.get(&new_candidate.block_hash).unwrap().state,
        PinStateV1::AwaitingJobFinalization {
            candidate: new_candidate
        }
    );

    drop(coordinator);
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(restarted.status(), RetentionStatus::Ready(_)));
    assert_eq!(
        inspect_retention_journal(root.path()).unwrap().records,
        survivors.into_iter().collect::<Vec<_>>()
    );
}
