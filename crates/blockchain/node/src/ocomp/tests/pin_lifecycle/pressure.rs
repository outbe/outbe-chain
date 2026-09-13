use super::*;

#[test]
fn durable_registry_tracks_more_than_256_independent_jobs_across_restart() {
    let source = Arc::new(DeterministicProofSource::default());
    let root = tempfile::tempdir().expect("multi-job journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    for ordinal in 0_u64..257 {
        let request = block(
            1_000 + ordinal,
            keccak256(ordinal.to_be_bytes()),
            u8::try_from(ordinal & 0xff).unwrap(),
        );
        coordinator
            .record_tentative(candidate(
                &request,
                keccak256((ordinal + 10_000).to_be_bytes()),
            ))
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
        let block_hash = keccak256([b"pressure-block".as_slice(), &ordinal.to_be_bytes()].concat());
        CandidatePinV1 {
            block_number: ordinal.saturating_add(1),
            block_hash,
            state_root: keccak256([b"pressure-state".as_slice(), &ordinal.to_be_bytes()].concat()),
            intent_id: keccak256([b"pressure-intent".as_slice(), &ordinal.to_be_bytes()].concat()),
            wwd: 7,
            ce_sealed_root: B256::repeat_byte(4),
            protocol_bundle_hash: B256::repeat_byte(3),
            input_lease_id: B256::repeat_byte(0x71),
        }
    }

    let root = tempfile::tempdir().expect("pressure journal root");
    let watermark = retention_pressure_watermark_for_test();
    let ancient_tentative = pressure_candidate(1);
    let finalized = pressure_candidate(2);
    let due_terminal = pressure_candidate(3);
    let future_terminal = pressure_candidate(4);
    let due_job = keccak256(b"pressure due job");
    let future_job = keccak256(b"pressure future job");
    let mut records = vec![
        (
            ancient_tentative.block_hash,
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::Tentative {
                    candidate: ancient_tentative,
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
                    job_id: (ordinal % 2 == 0).then(|| keccak256(ordinal.to_be_bytes())),
                    source_generation: (ordinal % 2 == 0).then_some(ordinal.saturating_add(1)),
                    reason: if ordinal % 2 == 0 {
                        PinReleaseReason::RetentionSatisfied
                    } else {
                        PinReleaseReason::Orphaned
                    },
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

    let source = Arc::new(DeterministicProofSource::default());
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
    let new_candidate = pressure_candidate(99_999);
    let compaction_durability = Arc::new(FailOnceDurability::disarmed(FailSync::File));
    let interrupted_compaction = OcompRetentionCoordinator::open_with_durability(
        root.path(),
        source.clone(),
        compaction_durability.clone(),
    );
    compaction_durability.arm();
    interrupted_compaction.set_closure_checkpoint_for_test(u64::MAX);
    assert!(interrupted_compaction
        .record_tentative(new_candidate)
        .is_err());
    assert!(root.path().join("pin.v1.tmp").is_file());
    drop(interrupted_compaction);
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    coordinator.set_closure_checkpoint_for_test(u64::MAX);
    assert!(matches!(coordinator.status(), RetentionStatus::Ready(_)));
    coordinator
        .record_tentative(new_candidate)
        .expect("recovered compacted admission is idempotent");

    let snapshot = inspect_retention_journal(root.path()).expect("inspect compacted journal");
    let survivors = snapshot.records.into_iter().collect::<BTreeMap<_, _>>();
    assert_eq!(survivors.len(), 4);
    assert_eq!(
        survivors.get(&ancient_tentative.block_hash).unwrap().state,
        PinStateV1::Tentative {
            candidate: ancient_tentative
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
        PinStateV1::Tentative {
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
