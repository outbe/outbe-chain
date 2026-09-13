use super::*;

#[derive(Clone)]
struct TransientFinalitySource {
    inner: DeterministicProofSource,
    resolve_calls: Arc<AtomicUsize>,
    failures_before_ready: usize,
}

impl FinalizedInputProofSource for TransientFinalitySource {
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
        if self.resolve_calls.fetch_add(1, Ordering::SeqCst) < self.failures_before_ready {
            return Err(RetentionError::Source(
                "finalized candidate header is not persisted yet".to_owned(),
            ));
        }
        self.inner.resolve_finality(candidate)
    }
}

#[derive(Default)]
struct RecordingSnapshotArmer {
    jobs: Mutex<Vec<B256>>,
}

impl FinalizedSnapshotArmer for RecordingSnapshotArmer {
    fn arm_finalized_snapshot(&self, job_id: B256) -> Result<(), String> {
        self.jobs
            .lock()
            .map_err(|_| "recording snapshot armer lock poisoned".to_owned())?
            .push(job_id);
        Ok(())
    }
}

#[test]
fn ocm_pin_001_missing_finality_keeps_the_job_non_signable_and_can_reconcile_later() {
    let request = block(100, B256::repeat_byte(0x30), 0);
    let candidate = candidate(&request, B256::repeat_byte(0x40));
    let job_id = B256::repeat_byte(0x50);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    source.set_finality_available(false);
    assert!(coordinator.reconcile_finalized(&request).is_err());
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        }
    );
    assert!(!coordinator.is_signable(job_id));
    assert!(!coordinator.is_exportable(job_id));

    source.set_finality_available(true);
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
    assert!(!coordinator.is_signable(job_id));
    assert!(coordinator.is_exportable(job_id));
}

#[test]
fn ocm_pin_001_old_tentative_survives_repeated_finality_misses_and_restart() {
    let request = block(100, B256::repeat_byte(0x72), 0x73);
    let candidate = candidate(&request, B256::repeat_byte(0x74));
    let job_id = B256::repeat_byte(0x75);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("delayed-finality journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    source.set_finality_available(false);
    for ordinal in 0_u64..9 {
        assert!(coordinator
            .reconcile_finalized(&block(
                200 + ordinal,
                keccak256(ordinal.to_be_bytes()),
                u8::try_from(ordinal).expect("bounded fixture ordinal"),
            ))
            .is_err());
    }
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        },
        "age and repeated reconciliation misses cannot evict unresolved Tentative state"
    );
    drop(coordinator);

    source.set_finality_available(true);
    source.observe_finalized(&request);
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    restarted
        .reconcile_finalized(&request)
        .expect("a later exact finality observation reconciles after restart");
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Finalized {
            job_id: current,
            ..
        } if current == job_id
    ));
}

#[tokio::test]
async fn ocm_pin_001_restart_reconciles_a_canonical_tentative_from_the_recovered_tip() {
    let request = block(100, B256::repeat_byte(0x38), 8);
    let successor = block_extending(101, B256::repeat_byte(0x39), request.block_hash(), 9);
    let candidate = candidate(&request, B256::repeat_byte(0x47));
    let job_id = B256::repeat_byte(0x57);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    drop(coordinator);

    source.observe_finalized(&successor);
    let restarted = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    let (service, handle, execution) =
        OcompRetentionService::new_with_execution_readiness(restarted.clone(), None, 99);
    let worker = tokio::spawn(service.run());
    handle
        .reconcile_finalized(&successor)
        .expect("recovered marshal tip is queued exactly");
    tokio::task::yield_now().await;
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Tentative { .. }
    ));

    execution
        .notify_execution_finalized(successor.number())
        .expect("recovered execution watermark reaches retention");
    drop(handle);
    drop(execution);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("recovered retention worker finishes")
        .expect("recovered retention worker task succeeds");
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
    assert!(!restarted.is_signable(job_id));
    assert!(restarted.is_exportable(job_id));
}

#[test]
fn ocm_pin_001_production_source_opens_typed_post_state_on_each_independent_node() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts: _pending_receipts,
    } = production_candidate_source();
    let roots = (0..4)
        .map(|_| tempfile::tempdir().expect("validator journal root"))
        .collect::<Vec<_>>();

    for root in &roots {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, &request),
            VoteOutcome::Positive
        );
        assert_eq!(
            ready_record(&coordinator),
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::Tentative { candidate },
            }
        );
        assert!(root.path().join("pin.v1").is_file());
    }
}

#[tokio::test]
async fn ocm_pin_001_consensus_finality_notification_does_no_proof_or_disk_work_inline() {
    let request = block(100, B256::repeat_byte(0x79), 0x7a);
    let candidate = candidate(&request, B256::repeat_byte(0x7b));
    let job_id = B256::repeat_byte(0x7c);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");
    source.observe_finalized(&request);
    let snapshot_armer = Arc::new(RecordingSnapshotArmer::default());
    let (service, handle) = OcompRetentionService::new_with_snapshot_armer(
        coordinator.clone(),
        Some(snapshot_armer.clone()),
    );

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the node-local finality notification");
    assert!(
        snapshot_armer
            .jobs
            .lock()
            .expect("recorded snapshot jobs")
            .is_empty(),
        "consensus finality notification must not arm the snapshot inline"
    );
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        }
    );

    drop(handle);
    service.run().await;
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
    assert_eq!(
        snapshot_armer
            .jobs
            .lock()
            .expect("recorded snapshot jobs")
            .as_slice(),
        &[job_id],
        "the node-owned worker must arm the exact finalized snapshot before returning"
    );
}

#[tokio::test]
async fn ocm_pin_001_finality_waits_for_exact_local_execution_without_dropping_the_block() {
    let request = block(100, B256::repeat_byte(0x8a), 0x8b);
    let candidate = candidate(&request, B256::repeat_byte(0x8c));
    let job_id = B256::repeat_byte(0x8d);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    source.observe_finalized(&request);
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");

    let (service, handle, execution) =
        OcompRetentionService::new_with_execution_readiness(coordinator.clone(), None, 99);
    let worker = tokio::spawn(service.run());

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the exact finalized block");
    tokio::task::yield_now().await;
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        },
        "finality must not read execution state before the exact block is locally ready"
    );

    execution
        .notify_execution_finalized(request.number())
        .expect("execution readiness reaches the retention worker");
    drop(handle);
    drop(execution);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("retention worker finishes after its inputs close")
        .expect("retention worker task succeeds");

    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        },
        "the same exact finalized block must reconcile once execution is ready"
    );
}

#[tokio::test]
async fn ocm_pin_001_worker_retries_transient_finalized_header_unavailability() {
    let request = block(100, B256::repeat_byte(0x7d), 0x7e);
    let candidate = candidate(&request, B256::repeat_byte(0x7f));
    let job_id = B256::repeat_byte(0x80);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    inner.observe_finalized(&request);
    let resolve_calls = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(TransientFinalitySource {
        inner,
        resolve_calls: resolve_calls.clone(),
        failures_before_ready: 1,
    });
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the node-local finality notification");
    drop(handle);
    service.run().await;

    assert_eq!(resolve_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
}

#[tokio::test]
async fn ocm_pin_001_worker_retains_exact_block_beyond_the_old_retry_limit() {
    let request = block(100, B256::repeat_byte(0x8e), 0x8f);
    let candidate = candidate(&request, B256::repeat_byte(0x90));
    let job_id = B256::repeat_byte(0x91);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    inner.observe_finalized(&request);
    let resolve_calls = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(TransientFinalitySource {
        inner,
        resolve_calls: resolve_calls.clone(),
        failures_before_ready: 8,
    });
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("exact finalized block is queued");
    drop(handle);
    tokio::time::timeout(Duration::from_secs(5), service.run())
        .await
        .expect("retention continues beyond the former eight-attempt horizon");

    assert_eq!(resolve_calls.load(Ordering::SeqCst), 9);
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Finalized {
            job_id: current,
            ..
        } if current == job_id
    ));
}

#[tokio::test]
async fn ocm_pin_001_queued_finality_does_not_skip_a_job_in_the_earlier_block() {
    let request = block(100, B256::repeat_byte(0x80), 0x81);
    let next = block_extending(101, B256::repeat_byte(0x82), request.block_hash(), 0x83);
    let candidate = candidate(&request, B256::repeat_byte(0x84));
    let job_id = B256::repeat_byte(0x85);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    source.observe_finalized(&request);
    source.observe_finalized(&next);
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("request-block finality is queued");
    handle
        .reconcile_finalized(&next)
        .expect("later finality is queued before the worker runs");
    assert_eq!(coordinator.status(), RetentionStatus::Empty);

    drop(handle);
    service.run().await;
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
}

#[tokio::test]
async fn ocm_pin_001_new_payload_pending_receipts_reach_the_production_journal_before_vote() {
    use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
    use reth_ethereum::node::api::BeaconEngineMessage;
    use reth_node_builder::{ConsensusEngineHandle, ExecutionPayload as _};

    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let execution_output = pending_receipts
        .lock()
        .expect("pending receipt fixture lock")
        .take()
        .expect("pending execution fixture");
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);
    let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = ConsensusEngineHandle::<OutbePayloadTypes>::new(engine_tx);

    let prepare = async {
        let payload = OutbeExecutionData::new(Arc::new(request.clone().into_inner()));
        let status = engine
            .new_payload(payload)
            .await
            .expect("execution engine responds to locally built payload");
        assert!(status.is_valid());
        coordinator
            .prepare_candidate(&request)
            .expect("production pending receipts and typed state are durable before vote");
    };
    let execute = async {
        let BeaconEngineMessage::NewPayload { payload, tx } = engine_rx
            .recv()
            .await
            .expect("locally built payload reaches the execution engine")
        else {
            panic!("candidate preparation must use new_payload");
        };
        assert_eq!(payload.block_hash(), request.block_hash());
        *pending_receipts
            .lock()
            .expect("pending receipt fixture lock") = Some(execution_output);
        tx.send(Ok(PayloadStatus::new(
            PayloadStatusEnum::Valid,
            Some(request.block_hash()),
        )))
        .expect("candidate preparation is still awaiting execution status");
    };

    futures::join!(prepare, execute);
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        }
    );
    assert!(root.path().join("pin.v1").is_file());
}

#[test]
fn ocm_pin_001_tentative_is_durable_across_independent_nodes_and_finalizes_exactly() {
    let request = block(100, B256::repeat_byte(0x31), 1);
    let candidate = candidate(&request, B256::repeat_byte(0x41));
    let job_id = B256::repeat_byte(0x51);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let roots = (0..4)
        .map(|_| tempfile::tempdir().expect("validator journal root"))
        .collect::<Vec<_>>();
    let coordinators = roots
        .iter()
        .map(|root| OcompRetentionCoordinator::open(root.path(), source.clone()))
        .collect::<Vec<_>>();

    let mut journal_bytes = Vec::new();
    for coordinator in &coordinators {
        assert_eq!(coordinator.status(), RetentionStatus::Empty);
        assert_eq!(
            DeterministicConsensusDriver::vote(coordinator, &request),
            VoteOutcome::Positive
        );
        assert_eq!(
            ready_record(coordinator),
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::Tentative { candidate },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        let bytes = fs::read(roots[journal_bytes.len()].path().join("pin.v1"))
            .expect("positive vote must follow a published journal");
        assert!(!bytes.is_empty());
        journal_bytes.push(bytes);
    }
    assert!(journal_bytes.windows(2).all(|pair| pair[0] == pair[1]));

    for coordinator in &coordinators {
        DeterministicConsensusDriver::finalize(source.as_ref(), coordinator, &request);
        assert_eq!(
            ready_record(coordinator),
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::Finalized {
                    candidate,
                    job_id,
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 114,
                },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        assert!(coordinator.is_exportable(job_id));
    }
}

#[test]
fn ocm_pin_001_orphan_releases_and_remains_non_signable_after_restart() {
    let request = block(100, B256::repeat_byte(0x32), 2);
    let canonical = block(100, B256::repeat_byte(0x33), 3);
    let candidate = candidate(&request, B256::repeat_byte(0x42));
    let job_id = B256::repeat_byte(0x52);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let roots = (0..4)
        .map(|_| tempfile::tempdir().expect("validator journal root"))
        .collect::<Vec<_>>();

    for root in &roots {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, &request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &canonical);
        assert_eq!(
            ready_record(&coordinator),
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::Released {
                    candidate,
                    job_id: None,
                    source_generation: None,
                    reason: PinReleaseReason::Orphaned,
                    observed_height: canonical.number(),
                    export: None,
                },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        drop(coordinator);

        let restarted = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
        assert!(!restarted.is_signable(job_id));
        assert!(!restarted.is_exportable(job_id));
        assert_eq!(
            DeterministicConsensusDriver::vote(&restarted, &request),
            VoteOutcome::Abstained,
            "the orphaned candidate must not become live after restart"
        );
    }
}

#[test]
fn tentative_pin_selects_exact_day_and_orphan_finality_queues_retained_gc() {
    let request = block(100, B256::repeat_byte(0x62), 0x63);
    let canonical = block(100, B256::repeat_byte(0x64), 0x65);
    let candidate = candidate(&request, B256::repeat_byte(0x66));
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .expect("fixture tentative JobId");
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("validator journal root");
    let storage = Arc::new(MemoryStorage::default());
    let retained_writer = Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone()));
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        retained_writer,
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    let day = WorldwideDay::new(candidate.wwd);
    let pin = RetainedTributePin {
        input_lease_id: candidate.input_lease_id,
        worldwide_day: day,
    };
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&coordinator, day).unwrap(),
        Some(pin)
    );

    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x67; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: alloy_primitives::Address::repeat_byte(0x68),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    };
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    repository.put(&tribute).expect("fixture current Tribute");
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("tentative retained copy");
    storage
        .apply_atomic(&retain)
        .expect("tentative retained transaction");
    repository
        .delete(tribute_id)
        .expect("fixture current retirement");

    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &canonical);
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::OrphanGcPending {
            candidate: orphaned,
            observed_height,
        } if orphaned == candidate && observed_height == canonical.number()
    ));
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&coordinator, day).unwrap(),
        None
    );
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 10)
            .expect("orphan retained source before background GC")
            .records
            .len(),
        1
    );
    coordinator
        .release_due(canonical.number())
        .expect("background orphan GC")
        .expect("orphan retained input is released");
    assert!(retained_reader
        .list_by_day(pin, None, 10)
        .expect("orphan retained source after background GC")
        .records
        .is_empty());
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Released {
            candidate: released,
            job_id: None,
            reason: PinReleaseReason::Orphaned,
            ..
        } if released == candidate
    ));
}
