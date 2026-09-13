use super::*;

#[derive(Clone)]
struct ResponseWindowCompletedSource {
    inner: DeterministicProofSource,
}

impl FinalizedInputProofSource for ResponseWindowCompletedSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        self.inner.candidate_for_block(block)
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        self.inner.resolve_finality(candidate)
    }

    fn terminal_height_at(
        &self,
        block: &ConsensusBlock,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        let Some(quorum_height) = self
            .inner
            .terminal
            .lock()
            .expect("deterministic terminal lock")
            .get(&job_id)
            .copied()
        else {
            return Ok(None);
        };
        let CandidateFinalityV1::Finalized(finalized) = self.inner.resolve_finality(candidate)?
        else {
            return Err(RetentionError::Source(
                "completed response-window fixture became orphaned".to_owned(),
            ));
        };
        retention_terminal_height_for_status(
            OcompJobStatus::Completed,
            block.number(),
            finalized.deadline_height,
            quorum_height,
        )
    }
}

#[test]
fn ocm_pin_001_finalized_state_drives_terminal_retention_after_export_ack() {
    let request = block(151, B256::repeat_byte(0x35), 5);
    let candidate = candidate(&request, B256::repeat_byte(0x45));
    let job_id = B256::repeat_byte(0x55);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("terminal observation journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x46))
        .expect("export ACK is durable before terminal retention");
    source.observe_terminal(job_id, 160);
    let terminal_block = block(160, B256::repeat_byte(0x36), 6);
    source.observe_finalized(&terminal_block);
    coordinator
        .reconcile_finalized(&terminal_block)
        .expect("finalized terminal state is node-local authority");

    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            terminal_height: 160,
            release_height: 224,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_terminal_state_retires_unexported_input_after_evidence_window() {
    let request = block(151, B256::repeat_byte(0x37), 7);
    let candidate = candidate(&request, B256::repeat_byte(0x47));
    let job_id = B256::repeat_byte(0x57);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("unexported terminal journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    source.observe_terminal(job_id, 160);
    coordinator
        .reconcile_finalized(&block(160, B256::repeat_byte(0x38), 8))
        .expect("terminal observation closes retention while exporter is unavailable");

    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            source_generation,
            export: None,
            terminal_height: 160,
            release_height: 224,
            ..
        } if current == job_id && source_generation == finalized.generation
    ));
    let discovery = coordinator
        .discovery_job_records(job_id)
        .expect("terminal job retains its exact discovery identity until retirement");
    assert_eq!(discovery.len(), 1);
    assert_eq!(discovery[0].0, finalized.generation);
    assert!(coordinator
        .confirm_export_ack(job_id, finalized.generation, 9, B256::repeat_byte(0x49),)
        .is_err());
    assert!(!coordinator.is_exportable(job_id));
    assert!(!coordinator.is_signable(job_id));
    drop(coordinator);
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            export: None,
            ..
        } if current == job_id
    ));
    assert!(coordinator
        .release_due(223)
        .expect("release scan")
        .is_none());
    coordinator
        .release_due(224)
        .expect("release at evidence-window boundary")
        .expect("terminal record released");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Released {
            job_id: Some(current),
            reason: PinReleaseReason::RetentionSatisfied,
            export: None,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_finalized_reconciliation_leaves_due_terminal_for_the_gc_worker() {
    let request = block(151, B256::repeat_byte(0x6A), 0x6B);
    let candidate = candidate(&request, B256::repeat_byte(0x6C));
    let job_id = B256::repeat_byte(0x6D);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("production-open journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x6F))
        .expect("export ACK is durable before terminal retention");
    source.observe_terminal(job_id, 160);
    coordinator
        .reconcile_finalized(&block(160, B256::repeat_byte(0x6E), 0x6F))
        .expect("canonical terminal state is observed");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            terminal_height: 160,
            release_height: 224,
            ..
        } if current == job_id
    ));

    coordinator
        .reconcile_finalized(&block(224, B256::repeat_byte(0x70), 0x71))
        .expect("finalized reconciliation does not execute retained-input GC");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            ..
        } if current == job_id
    ));
    coordinator
        .release_due(224)
        .expect("GC worker transition")
        .expect("due terminal is released by GC");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Released {
            job_id: Some(current),
            reason: PinReleaseReason::RetentionSatisfied,
            observed_height: 224,
            ..
        } if current == job_id
    ));
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Released {
            job_id: Some(current),
            reason: PinReleaseReason::RetentionSatisfied,
            observed_height: 224,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_completed_status_is_not_retention_terminal_before_response_deadline() {
    {
        let status = OcompJobStatus::Completed;
        assert_eq!(
            retention_terminal_height_for_status(status, 160, 165, 160).unwrap(),
            None,
            "quorum at 160 must not hide a still-open job before deadline 165"
        );
        assert_eq!(
            retention_terminal_height_for_status(status, 165, 165, 160).unwrap(),
            Some(165),
            "deadline closure, not quorum formation, terminalizes retention"
        );
    }
}

#[test]
fn ocm_pin_001_restart_keeps_quorum_complete_export_live_until_deadline() {
    let request = block(100, B256::repeat_byte(0x37), 7);
    let candidate = candidate(&request, B256::repeat_byte(0x47));
    let job_id = B256::repeat_byte(0x57);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    let source = Arc::new(ResponseWindowCompletedSource {
        inner: inner.clone(),
    });
    let root = tempfile::tempdir().expect("response-window journal root");

    {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, &request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(&inner, &coordinator, &request);
        let (generation, finalized) = coordinator
            .finalized_job_record(job_id)
            .expect("job reaches finalized state");
        assert_eq!(finalized.deadline_height, 114);
        coordinator
            .record_exported(job_id, generation, 9, B256::repeat_byte(0x67))
            .expect("job reaches exported state");

        inner.observe_terminal(job_id, 110);
        coordinator
            .reconcile_finalized(&block(110, B256::repeat_byte(0x38), 8))
            .expect("quorum block reconciles without terminalizing retention");
        assert_eq!(coordinator.finalized_live_jobs().unwrap().len(), 1);
        coordinator
            .exported_job_record(job_id)
            .expect("quorum-complete job remains exported before deadline");
    }

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    restarted
        .reconcile_finalized(&block(113, B256::repeat_byte(0x39), 9))
        .expect("restart before deadline restores the live export");
    assert_eq!(restarted.finalized_live_jobs().unwrap().len(), 1);
    restarted
        .exported_job_record(job_id)
        .expect("restarted node can still serve the pinned job");

    restarted
        .reconcile_finalized(&block(114, B256::repeat_byte(0x3A), 10))
        .expect("deadline closure terminalizes retention");
    assert!(restarted.finalized_live_jobs().unwrap().is_empty());
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Terminal {
            job_id: current,
            terminal_height: 114,
            ..
        } if current == job_id
    ));
}
