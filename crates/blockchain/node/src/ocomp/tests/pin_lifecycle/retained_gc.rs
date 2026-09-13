use super::*;

struct FailNthDurability {
    point: FailSync,
    remaining_calls: AtomicUsize,
}

struct FailFirstAtomicWrite {
    inner: Arc<MemoryStorage>,
    failed: AtomicBool,
}

struct AckDuringGcWrite {
    inner: Arc<MemoryStorage>,
    coordinator: Mutex<Option<std::sync::Weak<OcompRetentionCoordinator>>>,
    canonical: OcompJobRecordV1,
    export: ExportAuthorityV1,
    armed: AtomicBool,
}

impl StorageWriter for AckDuringGcWrite {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.inner.apply_atomic(batch)?;
        if self.armed.swap(false, Ordering::SeqCst) {
            self.coordinator
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap()
                .confirm_canonical_export_ack(&self.canonical, self.export)
                .unwrap();
        }
        Ok(())
    }
}

#[test]
fn gc_completion_rechecks_a_concurrent_canonical_ack_without_global_failure() {
    let fixture = production_candidate_source();
    let canonical = canonical_terminal_fixture(fixture.candidate, OcompJobStatus::Completed);
    let finalized = canonical.finalized.as_ref().unwrap();
    let export = ExportAuthorityV1 {
        source_generation: 2,
        lease_generation: 3,
        manifest_hash: B256::repeat_byte(0xaf),
    };
    let root = tempfile::tempdir().unwrap();
    seed_retention_journal_for_test(
        root.path(),
        8,
        fixture.candidate.block_hash,
        vec![(
            fixture.candidate.block_hash,
            PinRecordV1 {
                generation: 8,
                state: PinStateV1::GcPending {
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
            },
        )],
    )
    .unwrap();
    let storage = Arc::new(MemoryStorage::default());
    let writer = Arc::new(AckDuringGcWrite {
        inner: storage.clone(),
        coordinator: Mutex::new(None),
        canonical: canonical.clone(),
        export,
        armed: AtomicBool::new(true),
    });
    let coordinator = Arc::new(OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        fixture.source,
        Arc::new(RetainedTributeWriter::new(storage, writer.clone())),
    ));
    *writer.coordinator.lock().unwrap() = Some(Arc::downgrade(&coordinator));
    let mut schedule = RetainedGcRetrySchedule::default();
    let target = finalized.deadline_height + 64;
    let first = coordinator
        .run_gc_cycle_with_retry_for_test(target, Instant::now(), &mut schedule)
        .unwrap();
    assert!(!first.global_deferred);
    assert_eq!(first.completed, 0);
    assert!(
        !writer.armed.load(Ordering::SeqCst),
        "GC must cross the real storage write seam"
    );
    assert!(
        matches!(ready_record(&coordinator).state, PinStateV1::GcPending { export: Some(actual), .. } if actual == export)
    );
    let second = coordinator
        .run_gc_cycle_with_retry_for_test(target, Instant::now(), &mut schedule)
        .unwrap();
    assert_eq!(second.completed, 1);
    assert!(
        matches!(ready_record(&coordinator).state, PinStateV1::Released { export: Some(actual), .. } if actual == export)
    );
}

impl StorageWriter for FailFirstAtomicWrite {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        if !self.failed.swap(true, Ordering::SeqCst) {
            return Err(StorageError::Unavailable {
                source: Box::new(io::Error::other("injected retained GC outage")),
            });
        }
        self.inner.apply_atomic(batch)
    }
}

impl FailNthDurability {
    fn disarmed(point: FailSync) -> Self {
        Self {
            point,
            remaining_calls: AtomicUsize::new(0),
        }
    }

    fn arm(&self, fail_on_call: usize) {
        assert!(fail_on_call > 0, "fault injection call is one-based");
        self.remaining_calls.store(fail_on_call, Ordering::SeqCst);
    }

    fn should_fail(&self, point: FailSync) -> bool {
        if self.point != point {
            return false;
        }
        let mut remaining = self.remaining_calls.load(Ordering::SeqCst);
        while let Some(next) = remaining.checked_sub(1) {
            match self.remaining_calls.compare_exchange_weak(
                remaining,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return remaining == 1,
                Err(observed) => remaining = observed,
            }
        }
        false
    }
}

impl JournalDurability for FailNthDurability {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.should_fail(FailSync::File) {
            return Err(io::Error::other("injected numbered file fsync failure"));
        }
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> io::Result<()> {
        if self.should_fail(FailSync::Directory) {
            return Err(io::Error::other(
                "injected numbered directory fsync failure",
            ));
        }
        directory.sync_all()
    }
}

fn retain_fixture_tribute(
    storage: &Arc<MemoryStorage>,
    candidate: CandidatePinV1,
    marker: u8,
) -> (RetainedTributePin, RetainedTributeReader) {
    let (pin, retained_reader, retain) = fixture_tribute_retention(storage, candidate, marker);
    storage
        .apply_atomic(&retain)
        .expect("fixture retained transaction");
    (pin, retained_reader)
}

fn fixture_tribute_retention(
    storage: &Arc<MemoryStorage>,
    candidate: CandidatePinV1,
    marker: u8,
) -> (RetainedTributePin, RetainedTributeReader, AtomicWriteBatch) {
    fixture_tribute_retention_with_digest(storage, candidate, [marker; 32], marker)
}

fn fixture_tribute_retention_with_digest(
    storage: &Arc<MemoryStorage>,
    candidate: CandidatePinV1,
    digest: [u8; 32],
    owner_marker: u8,
) -> (RetainedTributePin, RetainedTributeReader, AtomicWriteBatch) {
    let day = WorldwideDay::new(candidate.wwd);
    let tribute_id = WwdEntityId::from_day_and_digest(day, digest);
    TributeRepositoryWriter::new(storage.clone(), storage.clone())
        .put(&TributeData {
            tribute_id,
            owner: Address::repeat_byte(owner_marker.wrapping_add(1)),
            worldwide_day: day,
            issuance_amount_minor: U256::from(10),
            issuance_currency: 840,
            nominal_amount_minor: U256::from(11),
            reference_currency: 978,
            tribute_price_minor: U256::from(12),
            exclude_from_intex_issuance: false,
        })
        .expect("fixture current Tribute");
    let pin = RetainedTributePin {
        input_lease_id: candidate.input_lease_id,
        worldwide_day: day,
    };
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("fixture retained copy");
    (pin, retained_reader, retain)
}

fn retain_fixture_tributes(storage: &Arc<MemoryStorage>, candidate: CandidatePinV1, count: usize) {
    for index in 0..count {
        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&(index as u64).to_be_bytes());
        let (_, _, retain) =
            fixture_tribute_retention_with_digest(storage, candidate, digest, 0xe1);
        storage
            .apply_atomic(&retain)
            .expect("fixture retained page member");
    }
}

#[test]
fn ocm_pin_001_retained_gc_wake_delay_prefers_100ms_progress_and_earlier_retries() {
    assert_eq!(
        retained_gc_next_wake_delay(true, None),
        Duration::from_millis(100)
    );
    assert_eq!(
        retained_gc_next_wake_delay(true, Some(Duration::from_secs(5))),
        Duration::from_millis(100)
    );
    assert_eq!(
        retained_gc_next_wake_delay(true, Some(Duration::from_millis(50))),
        Duration::from_millis(50)
    );
    assert_eq!(
        retained_gc_next_wake_delay(false, None),
        Duration::from_secs(1)
    );
}

#[test]
fn ocm_pin_001_terminal_to_gc_pending_fsync_failure_recovers_exact_transition() {
    let request = block(100, B256::repeat_byte(0xa1), 6);
    let candidate = candidate(&request, B256::repeat_byte(0xa2));
    let job_id = B256::repeat_byte(0xa3);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("Terminal to GcPending recovery root");
    let storage = Arc::new(MemoryStorage::default());
    let (pin, retained_reader) = retain_fixture_tribute(&storage, candidate, 0xa4);
    let durability = Arc::new(FailNthDurability::disarmed(FailSync::File));
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes_and_durability(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
        durability.clone(),
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .observe_terminal(job_id, finalized.generation, 120)
        .expect("canonical expiry becomes durable Terminal");

    durability.arm(1);
    assert!(matches!(
        coordinator.release_due(184),
        Err(RetentionError::Io {
            operation: "fsync temporary",
            ..
        })
    ));
    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Unavailable { .. }
    ));
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 10)
            .expect("retained source before durable GC claim")
            .records
            .len(),
        1,
        "Mongo GC must not run before GcPending is durably published"
    );
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source,
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::GcPending {
            job_id: current,
            source_generation,
            export: None,
            ..
        } if current == job_id && source_generation == finalized.generation
    ));
    restarted
        .release_due(184)
        .expect("recovered GcPending transition retries GC")
        .expect("recovered GcPending transition releases the job");
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Released {
            job_id: Some(current),
            source_generation: Some(source_generation),
            export: None,
            ..
        } if current == job_id && source_generation == finalized.generation
    ));
    assert!(retained_reader
        .list_by_day(pin, None, 10)
        .expect("retained source after recovered GC")
        .records
        .is_empty());
}

#[test]
fn ocm_pin_001_gc_pending_to_released_fsync_failure_recovers_exact_transition() {
    let request = block(100, B256::repeat_byte(0xb1), 6);
    let candidate = candidate(&request, B256::repeat_byte(0xb2));
    let job_id = B256::repeat_byte(0xb3);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("GcPending to Released recovery root");
    let storage = Arc::new(MemoryStorage::default());
    let (pin, retained_reader) = retain_fixture_tribute(&storage, candidate, 0xb4);
    let durability = Arc::new(FailNthDurability::disarmed(FailSync::File));
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes_and_durability(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
        durability.clone(),
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .observe_terminal(job_id, finalized.generation, 120)
        .expect("canonical expiry becomes durable Terminal");

    durability.arm(2);
    assert!(matches!(
        coordinator.release_due(184),
        Err(RetentionError::Io {
            operation: "fsync temporary",
            ..
        })
    ));
    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Unavailable { .. }
    ));
    assert!(
        retained_reader
            .list_by_day(pin, None, 10)
            .expect("retained source after completed Mongo GC")
            .records
            .is_empty(),
        "the injected second fsync must be the GcPending to Released publication"
    );
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source,
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage)),
    );
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Released {
            job_id: Some(current),
            source_generation: Some(source_generation),
            export: None,
            reason: PinReleaseReason::RetentionSatisfied,
            ..
        } if current == job_id && source_generation == finalized.generation
    ));
    assert_eq!(
        restarted
            .release_due(184)
            .expect("Released replay is inert"),
        None
    );
    assert!(restarted
        .confirm_export_ack(job_id, finalized.generation, 9, B256::repeat_byte(0xb6))
        .is_err());
}

#[test]
fn ocm_pin_001_gc_pending_survives_mongo_failure_and_releases_without_export_ack() {
    let request = block(100, B256::repeat_byte(0x76), 6);
    let candidate = candidate(&request, B256::repeat_byte(0x77));
    let job_id = B256::repeat_byte(0x78);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("GC recovery journal root");
    let storage = Arc::new(MemoryStorage::default());
    let day = WorldwideDay::new(candidate.wwd);
    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x79; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: Address::repeat_byte(0x7A),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    TributeRepositoryWriter::new(storage.clone(), storage.clone())
        .put(&tribute)
        .expect("fixture current Tribute");
    let pin = RetainedTributePin {
        input_lease_id: candidate.input_lease_id,
        worldwide_day: day,
    };
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("fixture retained copy");
    storage
        .apply_atomic(&retain)
        .expect("fixture retained transaction");
    let failing_writer = Arc::new(FailFirstAtomicWrite {
        inner: storage.clone(),
        failed: AtomicBool::new(false),
    });
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), failing_writer)),
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .observe_terminal(job_id, finalized.generation, 120)
        .expect("canonical expiry becomes durable Terminal");
    assert!(matches!(
        coordinator.release_due(184),
        Err(RetentionError::RetainedTributeGc(_))
    ));
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::GcPending {
            job_id: current,
            source_generation,
            export: None,
            ..
        } if current == job_id && source_generation == finalized.generation
    ));
    assert!(coordinator
        .confirm_export_ack(job_id, finalized.generation, 9, B256::repeat_byte(0x7E))
        .is_err());
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 10)
            .expect("retained source after failed GC")
            .records
            .len(),
        1
    );
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source,
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );
    restarted
        .release_due(184)
        .expect("restart retries durable GcPending")
        .expect("restart completes retained-input GC");
    assert_eq!(
        restarted
            .released_job_authority(job_id)
            .expect("read released authority")
            .expect("canonical expiry authority remains durable")
            .export,
        None
    );
    assert!(restarted
        .confirm_export_ack(job_id, finalized.generation, 9, B256::repeat_byte(0x7E))
        .is_err());
    assert!(retained_reader
        .list_by_day(pin, None, 10)
        .expect("retained source after successful retry")
        .records
        .is_empty());
}

#[test]
fn ocm_pin_001_background_gc_recovers_due_work_from_the_durable_journal() {
    let request = block(100, B256::repeat_byte(0x7B), 6);
    let candidate = candidate(&request, B256::repeat_byte(0x7C));
    let job_id = B256::repeat_byte(0x7D);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("background GC journal root");
    let storage = Arc::new(MemoryStorage::default());
    let coordinator = Arc::new(OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage)),
    ));
    assert_eq!(
        DeterministicConsensusDriver::vote(coordinator.as_ref(), &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), coordinator.as_ref(), &request);
    let finalized = ready_record(coordinator.as_ref());
    coordinator
        .observe_terminal(job_id, finalized.generation, 120)
        .expect("canonical expiry becomes durable Terminal");

    let selector = SharedOcompRetentionSelector::new();
    selector
        .install(Arc::clone(&coordinator))
        .expect("install retention GC worker");
    selector.notify_finalized_height(184);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(
            ready_record(coordinator.as_ref()).state,
            PinStateV1::Released {
                job_id: Some(current),
                source_generation: Some(source_generation),
                export: None,
                ..
            } if current == job_id && source_generation == finalized.generation
        ) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "journal-driven background GC did not release due work"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ocm_pin_001_global_storage_failure_aborts_the_cycle_before_other_gc_work() {
    let first_request = block(100, B256::repeat_byte(0x81), 6);
    let second_request = block(101, B256::repeat_byte(0x82), 7);
    let mut first_candidate = candidate(&first_request, B256::repeat_byte(0x83));
    let mut second_candidate = candidate(&second_request, B256::repeat_byte(0x84));
    first_candidate.input_lease_id = B256::repeat_byte(0x91);
    second_candidate.input_lease_id = B256::repeat_byte(0x92);
    let first_job = B256::repeat_byte(0x85);
    let second_job = B256::repeat_byte(0x86);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job),
        (second_candidate, second_job),
    ]));
    let root = tempfile::tempdir().expect("fair GC journal root");
    let storage = Arc::new(MemoryStorage::default());
    let failing_writer = Arc::new(FailFirstAtomicWrite {
        inner: storage.clone(),
        failed: AtomicBool::new(false),
    });
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage, failing_writer)),
    );

    for (request, job, terminal_height) in [
        (&first_request, first_job, 120),
        (&second_request, second_job, 121),
    ] {
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, request);
        let finalized = ready_record(&coordinator);
        coordinator
            .observe_terminal(job, finalized.generation, terminal_height)
            .expect("terminal job enters durable GC queue");
    }

    let started_at = Instant::now();
    let mut retry_schedule = RetainedGcRetrySchedule::default();
    let failure_at = started_at + Duration::from_secs(2);
    assert!(matches!(
        coordinator.run_gc_cycle_with_retry_clock_for_test(
            185,
            started_at,
            failure_at,
            &mut retry_schedule,
        ),
        Err(RetentionError::RetainedTributeGc(_))
    ));
    let states = inspect_retention_journal(root.path())
        .expect("inspect globally failed GC cycle")
        .records;
    assert_eq!(
        states
            .iter()
            .filter(|(_, record)| matches!(record.state, PinStateV1::GcPending { .. }))
            .count(),
        1
    );
    assert_eq!(
        states
            .iter()
            .filter(|(_, record)| matches!(record.state, PinStateV1::Terminal { .. }))
            .count(),
        1
    );
    let deferred = coordinator
        .run_gc_cycle_with_retry_for_test(
            185,
            failure_at + Duration::from_millis(100),
            &mut retry_schedule,
        )
        .expect("closure wake cannot bypass global storage backoff");
    assert!(deferred.global_deferred);
    assert_eq!(deferred.pending, 0);
    assert_eq!(deferred.completed, 0);
    assert_eq!(deferred.pages, 0);
    assert_eq!(deferred.item_failures, 0);
    assert_eq!(deferred.retry_entries, 0);

    let recovered = coordinator
        .run_gc_cycle_with_retry_for_test(
            185,
            failure_at + Duration::from_secs(5),
            &mut retry_schedule,
        )
        .expect("global storage recovery retries durable work at its deadline");
    assert!(!recovered.global_deferred);
    assert_eq!(recovered.pending, 2);
    assert_eq!(recovered.completed, 2);
    assert_eq!(recovered.pages, 0);
    assert_eq!(recovered.item_failures, 0);
    assert_eq!(recovered.retry_entries, 0);
}

#[test]
fn ocm_pin_001_poisoned_gc_work_observes_its_own_backoff_while_healthy_work_progresses() {
    let poisoned_request = block(100, B256::repeat_byte(0xc1), 6);
    let healthy_request = block(101, B256::repeat_byte(0xc2), 7);
    let mut poisoned_candidate = candidate(&poisoned_request, B256::repeat_byte(0xc3));
    let mut healthy_candidate = candidate(&healthy_request, B256::repeat_byte(0xc4));
    poisoned_candidate.input_lease_id = B256::repeat_byte(0xd1);
    healthy_candidate.input_lease_id = B256::repeat_byte(0xd2);
    let poisoned_job = B256::repeat_byte(0xc5);
    let healthy_job = B256::repeat_byte(0xc6);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (poisoned_candidate, poisoned_job),
        (healthy_candidate, healthy_job),
    ]));
    let root = tempfile::tempdir().expect("per-work retry journal root");
    let storage = Arc::new(MemoryStorage::default());
    let (_, _, poisoned_retain) = fixture_tribute_retention(&storage, poisoned_candidate, 0xd3);
    let poisoned_index_repair =
        AtomicWriteBatch::from_operations(vec![poisoned_retain.operations()[1].clone()]);
    storage
        .apply_atomic(&AtomicWriteBatch::from_operations(vec![poisoned_retain
            .operations()[0]
            .clone()]))
        .expect("fixture poisoned retained body without its index");
    let release_page_limit =
        usize::try_from(OCOMP_POC_CANDIDATE_LIMITS_V1.max_tributes_per_work_shard)
            .expect("generated retained release page limit");
    retain_fixture_tributes(&storage, healthy_candidate, release_page_limit + 1);
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );

    for (request, job, terminal_height) in [
        (&poisoned_request, poisoned_job, 120),
        (&healthy_request, healthy_job, 121),
    ] {
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, request);
        let finalized = ready_record(&coordinator);
        coordinator
            .observe_terminal(job, finalized.generation, terminal_height)
            .expect("terminal job enters durable GC queue");
    }

    let started_at = Instant::now();
    let mut retry_schedule = RetainedGcRetrySchedule::default();
    let first = coordinator
        .run_gc_cycle_with_retry_for_test(185, started_at, &mut retry_schedule)
        .expect("first fair GC cycle");
    assert!(!first.global_deferred);
    assert_eq!(first.pending, 2);
    assert_eq!(first.completed, 0);
    assert_eq!(first.pages, 1);
    assert_eq!(first.item_failures, 1);
    assert_eq!(first.deferred, 1);
    assert_eq!(first.retry_entries, 1);

    let early = coordinator
        .run_gc_cycle_with_retry_for_test(
            185,
            started_at + Duration::from_millis(100),
            &mut retry_schedule,
        )
        .expect("healthy progress wake cannot bypass poison backoff");
    assert!(!early.global_deferred);
    assert_eq!(early.pending, 2);
    assert_eq!(early.completed, 1);
    assert_eq!(early.pages, 0);
    assert_eq!(early.item_failures, 0);
    assert_eq!(early.deferred, 1);
    assert_eq!(early.retry_entries, 1);

    let before_deadline = coordinator
        .run_gc_cycle_with_retry_for_test(
            185,
            started_at + Duration::from_millis(4_999),
            &mut retry_schedule,
        )
        .expect("finalized wake before the deadline remains deferred");
    assert!(!before_deadline.global_deferred);
    assert_eq!(before_deadline.pending, 1);
    assert_eq!(before_deadline.completed, 0);
    assert_eq!(before_deadline.pages, 0);
    assert_eq!(before_deadline.item_failures, 0);
    assert_eq!(before_deadline.deferred, 1);
    assert_eq!(before_deadline.retry_entries, 1);

    storage
        .apply_atomic(&poisoned_index_repair)
        .expect("repair poisoned retained index");
    let recovered = coordinator
        .run_gc_cycle_with_retry_for_test(
            185,
            started_at + Duration::from_secs(5),
            &mut retry_schedule,
        )
        .expect("poisoned work retries at its deadline");
    assert!(!recovered.global_deferred);
    assert_eq!(recovered.pending, 1);
    assert_eq!(recovered.completed, 1);
    assert_eq!(recovered.pages, 0);
    assert_eq!(recovered.item_failures, 0);
    assert_eq!(recovered.deferred, 0);
    assert_eq!(recovered.retry_entries, 0);
}

#[test]
fn ocm_pin_001_retained_predecessor_does_not_block_a_later_independent_job() {
    let first_request = block(151, B256::repeat_byte(0x31), 1);
    let later_request = block(221, B256::repeat_byte(0x32), 2);
    let first_candidate = candidate(&first_request, B256::repeat_byte(0x41));
    let later_candidate = candidate(&later_request, B256::repeat_byte(0x42));
    let first_job_id = B256::repeat_byte(0x51);
    let later_job_id = B256::repeat_byte(0x52);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (later_candidate, later_job_id),
    ]));
    let root = tempfile::tempdir().expect("multi-job journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &first_request);
    let first_finalized = ready_record(&coordinator);
    let first_exported = coordinator
        .record_exported(
            first_job_id,
            first_finalized.generation,
            9,
            B256::repeat_byte(0x61),
        )
        .expect("first job export is durable");
    let first_terminal = coordinator
        .observe_terminal(first_job_id, first_exported.generation, 219)
        .expect("first job reaches terminal retention");
    assert_eq!(first_terminal.generation, 4);

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &later_request),
        VoteOutcome::Positive,
        "a retained predecessor must not be a global OCOMP lock"
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &later_request);
    assert!(coordinator.is_exportable(later_job_id));
    assert_eq!(
        coordinator
            .observe_terminal(first_job_id, first_terminal.generation, 219)
            .expect("the retained predecessor remains independently addressable"),
        first_terminal
    );
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(restarted.is_exportable(later_job_id));
    assert_eq!(
        restarted
            .observe_terminal(first_job_id, first_terminal.generation, 219)
            .expect("both Job entries survive restart"),
        first_terminal
    );
}

#[test]
fn ocm_pin_001_shared_input_lease_is_collected_after_its_last_job_reference() {
    let first_request = block(151, B256::repeat_byte(0x33), 3);
    let later_request = block(221, B256::repeat_byte(0x34), 4);
    let first_candidate = candidate(&first_request, B256::repeat_byte(0x43));
    let later_candidate = candidate(&later_request, B256::repeat_byte(0x44));
    assert_eq!(
        first_candidate.input_lease_id, later_candidate.input_lease_id,
        "fixture models independent journal records sharing one retained input lease"
    );
    let first_job_id = B256::repeat_byte(0x53);
    let later_job_id = B256::repeat_byte(0x54);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (later_candidate, later_job_id),
    ]));
    let root = tempfile::tempdir().expect("shared-lease journal root");
    let storage = Arc::new(MemoryStorage::default());
    let day = WorldwideDay::new(first_candidate.wwd);
    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x61; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: alloy_primitives::Address::repeat_byte(0x62),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    repository.put(&tribute).expect("fixture current Tribute");
    let pin = RetainedTributePin {
        input_lease_id: first_candidate.input_lease_id,
        worldwide_day: day,
    };
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("fixture retained input lease");
    storage
        .apply_atomic(&retain)
        .expect("fixture retained transaction");
    repository
        .delete(tribute_id)
        .expect("fixture current retirement");
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &first_request);
    let first_finalized = ready_record(&coordinator);
    let first_exported = coordinator
        .record_exported(
            first_job_id,
            first_finalized.generation,
            9,
            B256::repeat_byte(0x91),
        )
        .expect("first export");
    coordinator
        .observe_terminal(first_job_id, first_exported.generation, 219)
        .expect("first terminal");
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &later_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &later_request);
    let later_finalized = ready_record(&coordinator);
    let later_exported = coordinator
        .record_exported(
            later_job_id,
            later_finalized.generation,
            10,
            B256::repeat_byte(0x92),
        )
        .expect("later independent export");

    coordinator
        .release_due(283)
        .expect("first release")
        .expect("first reference is due");
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 1)
            .expect("shared lease remains readable")
            .records
            .len(),
        1,
        "the predecessor cannot collect input still referenced by another job"
    );

    coordinator
        .observe_terminal(later_job_id, later_exported.generation, 300)
        .expect("later independent terminal");
    coordinator
        .release_due(364)
        .expect("last release")
        .expect("last reference is due");
    assert!(retained_reader
        .list_by_day(pin, None, 1)
        .expect("lease after last-reference GC")
        .records
        .is_empty());
}
