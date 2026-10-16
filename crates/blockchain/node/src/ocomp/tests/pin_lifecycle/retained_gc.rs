//! Release the local journal pin without an entity body store.
use super::*;

#[test]
fn journal_release_wake_delay_follows_progress() {
    assert_eq!(
        retained_gc_next_wake_delay(true),
        Duration::from_millis(100)
    );
    assert_eq!(retained_gc_next_wake_delay(false), Duration::from_secs(1));
}

#[test]
fn journal_release_recovers_each_durability_failure_and_replays_once() {
    for failure in [FailSync::File, FailSync::Directory] {
        let request = block(100, B256::repeat_byte(0xa1), 6);
        let candidate = candidate(&request);
        let job_id = fixture_job_id(candidate);
        let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
        let root = tempfile::tempdir().unwrap();
        let durability = Arc::new(FailOnceDurability::disarmed(failure));
        let coordinator = OcompRetentionCoordinator::open_with_durability(
            root.path(),
            source.clone(),
            durability.clone(),
        );
        FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &request).unwrap();
        FinalizedFrameDriver::bind(source.as_ref(), &coordinator, &request);
        let finalized = ready_record(&coordinator);
        let terminal = coordinator
            .observe_terminal(job_id, finalized.generation, 120)
            .unwrap();
        assert!(coordinator.release_due(183).unwrap().is_none());
        durability.arm();
        assert!(matches!(
            coordinator.release_due(184),
            Err(RetentionError::Io { .. })
        ));
        assert!(matches!(
            coordinator.status(),
            RetentionStatus::Unavailable { .. }
        ));
        drop(coordinator);

        let restarted = OcompRetentionCoordinator::open(root.path(), source);
        restarted.release_due(184).unwrap();
        let released = ready_record(&restarted);
        assert_eq!(released.generation, terminal.generation + 1);
        assert!(matches!(released.state, PinStateV1::Released {
            job_id: actual, source_generation, export: None, ..
        } if actual == job_id && source_generation == finalized.generation));
        let before = fs::read(root.path().join("pin.v1")).unwrap();
        assert!(restarted.release_due(184).unwrap().is_none());
        assert_eq!(fs::read(root.path().join("pin.v1")).unwrap(), before);
    }
}

#[test]
fn background_worker_releases_due_journal_pins_after_restart() {
    let request = block(100, B256::repeat_byte(0x7B), 6);
    let candidate = candidate(&request);
    let job_id = fixture_job_id(candidate);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("background pin release journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
    assert!(FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &request).is_ok());
    FinalizedFrameDriver::bind(source.as_ref(), coordinator.as_ref(), &request);
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
                job_id: current,
                source_generation,
                export: None,
                ..
            } if current == job_id && source_generation == finalized.generation
        ) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "journal-driven background pin release did not release due work"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
