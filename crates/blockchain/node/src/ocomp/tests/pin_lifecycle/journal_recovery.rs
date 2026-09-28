use super::*;

struct RecoverableDurabilityOutage {
    point: FailSync,
    unavailable: AtomicBool,
}

impl RecoverableDurabilityOutage {
    fn at(point: FailSync) -> Self {
        Self {
            point,
            unavailable: AtomicBool::new(false),
        }
    }

    fn fail(&self) {
        self.unavailable.store(true, Ordering::SeqCst);
    }

    fn restore(&self) {
        self.unavailable.store(false, Ordering::SeqCst);
    }

    fn should_fail(&self, point: FailSync) -> bool {
        self.point == point && self.unavailable.load(Ordering::SeqCst)
    }
}

impl JournalDurability for RecoverableDurabilityOutage {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.should_fail(FailSync::File) {
            return Err(io::Error::other("injected recoverable file fsync outage"));
        }
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> io::Result<()> {
        if self.should_fail(FailSync::Directory) {
            return Err(io::Error::other(
                "injected recoverable directory fsync outage",
            ));
        }
        directory.sync_all()
    }
}

#[test]
fn ocm_pin_001_crash_recovery_multi_job_and_ambiguous_restart_are_safe() {
    let first = block(100, B256::repeat_byte(0x34), 4);
    let second = block(101, B256::repeat_byte(0x35), 5);
    let first_candidate = candidate(&first);
    let second_candidate = candidate(&second);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, fixture_job_id(first_candidate)),
        (second_candidate, fixture_job_id(second_candidate)),
    ]));
    let fsync_root = tempfile::tempdir().expect("fsync journal root");
    let coordinator = OcompRetentionCoordinator::open_with_durability(
        fsync_root.path(),
        source.clone(),
        Arc::new(FailOnceDurability::at(FailSync::File)),
    );
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &first).is_err());
    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Unavailable { .. }
    ));
    assert!(fsync_root.path().join("pin.v1.tmp").is_file());
    assert!(!fsync_root.path().join("pin.v1").exists());
    drop(coordinator);
    let restarted_after_fsync = OcompRetentionCoordinator::open(fsync_root.path(), source.clone());
    assert!(matches!(
        restarted_after_fsync.status(),
        RetentionStatus::Ready(_)
    ));
    assert!(
        FinalizedFrameDriver::admit(source.as_ref(), &restarted_after_fsync, &first).is_ok(),
        "a complete exact-next temp generation must recover after restart"
    );

    let successor_root = tempfile::tempdir().expect("successor recovery root");
    let initial = OcompRetentionCoordinator::open(successor_root.path(), source.clone());
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &initial, &first).is_ok());
    drop(initial);
    let successor_durability = Arc::new(FailOnceDurability::disarmed(FailSync::File));
    let interrupted_successor = OcompRetentionCoordinator::open_with_durability(
        successor_root.path(),
        source.clone(),
        successor_durability.clone(),
    );
    successor_durability.arm();
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &interrupted_successor, &second).is_err());
    assert!(successor_root.path().join("pin.v1").is_file());
    assert!(successor_root.path().join("pin.v1.tmp").is_file());
    drop(interrupted_successor);
    let recovered_successor =
        OcompRetentionCoordinator::open(successor_root.path(), source.clone());
    assert!(matches!(
        recovered_successor.status(),
        RetentionStatus::Ready(_)
    ));
    assert!(
        FinalizedFrameDriver::admit(source.as_ref(), &recovered_successor, &second).is_ok(),
        "a valid temp successor must atomically replace the prior generation"
    );

    let root = tempfile::tempdir().expect("conflict journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &first).is_ok());
    let before_second_job = fs::read(root.path().join("pin.v1")).unwrap();
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &second).is_ok());
    let after_second_job = fs::read(root.path().join("pin.v1")).unwrap();
    assert_ne!(
        after_second_job, before_second_job,
        "an independent candidate must be added durably"
    );
    assert!(
        FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &first).is_ok(),
        "adding another Job must not replace the first Job entry"
    );
    assert_eq!(
        fs::read(root.path().join("pin.v1")).unwrap(),
        after_second_job,
        "an exact retry remains idempotent"
    );
    drop(coordinator);

    fs::write(root.path().join("pin.v1.tmp"), b"torn").expect("inject torn write");
    let restarted = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert!(matches!(restarted.status(), RetentionStatus::Ready(_)));
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &restarted, &first).is_ok());
    assert!(!root.path().join("pin.v1.tmp").exists());
    assert_eq!(
        fs::read(root.path().join("pin.v1")).unwrap(),
        after_second_job,
        "discarding an unpublished torn temp must preserve the last durable record"
    );

    let directory_fsync_root = tempfile::tempdir().expect("directory fsync journal root");
    let directory_fsync = OcompRetentionCoordinator::open_with_durability(
        directory_fsync_root.path(),
        Arc::new(DeterministicProofSource::with_jobs([(
            first_candidate,
            fixture_job_id(first_candidate),
        )])),
        Arc::new(FailOnceDurability::at(FailSync::Directory)),
    );
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &directory_fsync, &first).is_err());
    assert!(directory_fsync_root.path().join("pin.v1").is_file());
    assert!(matches!(
        directory_fsync.status(),
        RetentionStatus::Unavailable { .. }
    ));
}

#[test]
fn ocm_pin_001_transient_journal_io_recovers_in_process_without_restart() {
    for point in [FailSync::File, FailSync::Directory] {
        let request = block(100, B256::repeat_byte(0x36), 6);
        let candidate = candidate(&request);
        let source = Arc::new(DeterministicProofSource::with_jobs([(
            candidate,
            fixture_job_id(candidate),
        )]));
        let root = tempfile::tempdir().expect("recoverable journal root");
        let durability = Arc::new(RecoverableDurabilityOutage::at(point));
        let coordinator = Arc::new(OcompRetentionCoordinator::open_with_durability(
            root.path(),
            source.clone(),
            durability.clone(),
        ));
        let selector = SharedOcompRetentionSelector::new();
        selector
            .install(Arc::clone(&coordinator))
            .expect("install journal recovery worker");
        durability.fail();

        assert!(
            FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &request).is_err(),
            "a candidate must not be acknowledged while its journal write is not durable"
        );
        assert!(matches!(
            coordinator.status(),
            RetentionStatus::Unavailable { .. }
        ));
        selector.notify_finalized_height(request.number());

        durability.restore();
        selector.notify_finalized_height(request.number());
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        loop {
            match coordinator.status() {
                RetentionStatus::Ready(_) => break,
                RetentionStatus::Unavailable { .. } => {}
                RetentionStatus::Quarantined { reason } => {
                    panic!("recoverable journal outage became quarantined: {reason}")
                }
                RetentionStatus::Empty => {}
            }
            assert!(
                std::time::Instant::now() < deadline,
                "journal did not recover in process after storage was restored"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &request).is_ok(),
            "the same candidate must resume after durable journal recovery"
        );
    }
}

#[test]
fn ocm_pin_001_existing_export_authority_fails_closed_during_journal_recovery() {
    let first = block(100, B256::repeat_byte(0x39), 8);
    let second = block(101, B256::repeat_byte(0x3a), 9);
    let first_candidate = candidate(&first);
    let second_candidate = candidate(&second);
    let first_job = fixture_job_id(first_candidate);
    let second_job = fixture_job_id(second_candidate);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job),
        (second_candidate, second_job),
    ]));
    let root = tempfile::tempdir().expect("recoverable journal root");
    let durability = Arc::new(RecoverableDurabilityOutage::at(FailSync::File));
    let coordinator = Arc::new(OcompRetentionCoordinator::open_with_durability(
        root.path(),
        source.clone(),
        durability.clone(),
    ));
    let selector = SharedOcompRetentionSelector::new();
    selector
        .install(Arc::clone(&coordinator))
        .expect("install journal recovery worker");

    assert!(FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &first).is_ok());
    FinalizedFrameDriver::bind(source.as_ref(), coordinator.as_ref(), &first);
    let source_generation = coordinator
        .finalized_job_record(first_job)
        .expect("finalized source generation")
        .0;
    coordinator
        .record_exported(first_job, source_generation, 9, B256::repeat_byte(0x69))
        .expect("export authority");
    assert!(coordinator.is_signable(first_job));

    durability.fail();
    assert!(FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &second).is_err());
    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Unavailable { .. }
    ));
    assert!(!coordinator.is_signable(first_job));
    assert!(!coordinator.is_exportable(first_job));
    assert!(matches!(
        coordinator.discovery_job_records(first_job),
        Err(RetentionError::JournalUnavailable { .. })
    ));
    assert!(matches!(
        coordinator.confirm_export_ack(first_job, source_generation, 9, B256::repeat_byte(0x69),),
        Err(RetentionError::JournalUnavailable { .. })
    ));

    selector.notify_finalized_height(second.number());
    durability.restore();
    selector.notify_finalized_height(second.number());
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while !matches!(coordinator.status(), RetentionStatus::Ready(_)) {
        assert!(
            std::time::Instant::now() < deadline,
            "journal did not recover while prior export authority was withheld"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(coordinator.is_signable(first_job));
}

#[test]
fn ocm_pin_001_ambiguous_journal_stays_quarantined_without_automatic_mutation() {
    let first = block(100, B256::repeat_byte(0x37), 6);
    let second = block(101, B256::repeat_byte(0x38), 7);
    let first_candidate = candidate(&first);
    let second_candidate = candidate(&second);
    let root = tempfile::tempdir().expect("ambiguous journal root");
    seed_retention_journal_for_test(
        root.path(),
        1,
        first_candidate.block_hash,
        vec![(
            first_candidate.block_hash,
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::AwaitingJobFinalization {
                    candidate: first_candidate,
                },
            },
        )],
    )
    .expect("seed authoritative journal");
    let conflicting = tempfile::tempdir().expect("conflicting successor root");
    seed_retention_journal_for_test(
        conflicting.path(),
        2,
        second_candidate.block_hash,
        vec![(
            second_candidate.block_hash,
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::AwaitingJobFinalization {
                    candidate: second_candidate,
                },
            },
        )],
    )
    .expect("seed non-exact successor");
    fs::copy(
        conflicting.path().join("pin.v1"),
        root.path().join("pin.v1.tmp"),
    )
    .expect("install non-exact temporary successor");
    let journal_before = fs::read(root.path().join("pin.v1")).expect("read journal before open");
    let temporary_before =
        fs::read(root.path().join("pin.v1.tmp")).expect("read temporary before open");

    let coordinator = Arc::new(OcompRetentionCoordinator::open(
        root.path(),
        Arc::new(DeterministicProofSource::default()),
    ));
    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Quarantined { .. }
    ));
    let selector = SharedOcompRetentionSelector::new();
    selector
        .install(Arc::clone(&coordinator))
        .expect("install quarantined journal worker");
    std::thread::sleep(Duration::from_millis(50));

    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Quarantined { .. }
    ));
    assert_eq!(
        fs::read(root.path().join("pin.v1")).expect("read preserved journal"),
        journal_before
    );
    assert_eq!(
        fs::read(root.path().join("pin.v1.tmp")).expect("read preserved temporary"),
        temporary_before
    );
}

#[test]
fn ocm_pin_001_journal_recovery_backoff_is_capped_without_an_attempt_limit() {
    let seconds = (0..9)
        .map(|failure| journal_recovery_backoff(failure).as_secs())
        .collect::<Vec<_>>();
    assert_eq!(seconds, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
    assert_eq!(journal_recovery_backoff(u32::MAX), Duration::from_secs(60));
}

#[test]
fn ocm_pin_001_journal_bytes_are_stable_and_corruption_quarantines() {
    let request = block(100, B256::repeat_byte(0x37), 7);
    let candidate = candidate(&request);
    let job_id = fixture_job_id(candidate);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &request).is_ok());
    let record = ready_record(&coordinator);
    let bytes = fs::read(root.path().join("pin.v1")).unwrap();
    assert_eq!(
        keccak256_for_test(&bytes),
        b256!("d922d745db0256e979f508f1f9b581061fdebc45e04dc908faef256583da30bf"),
        "update only when the intentional journal wire format changes"
    );
    assert_eq!(record.generation, 1);
    drop(coordinator);

    let mut unsupported = bytes;
    unsupported[8..10].copy_from_slice(&5_u16.to_be_bytes());
    let body_len = unsupported.len() - 32;
    let checksum = alloy_primitives::keccak256(&unsupported[..body_len]);
    unsupported[body_len..].copy_from_slice(checksum.as_slice());
    fs::write(root.path().join("pin.v1"), unsupported).unwrap();
    let restarted = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert!(matches!(
        restarted.status(),
        RetentionStatus::Quarantined { .. }
    ));
    assert!(!restarted.is_signable(job_id));
}

fn keccak256_for_test(bytes: &[u8]) -> B256 {
    alloy_primitives::keccak256(bytes)
}
