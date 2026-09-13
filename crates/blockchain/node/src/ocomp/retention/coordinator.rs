use crate::ocomp::retention::*;

pub(in crate::ocomp::retention) const JOURNAL_RECORD_PRESSURE_WATERMARK: usize =
    JOURNAL_RECORD_COUNT_MAX - JOURNAL_RECORD_COUNT_MAX / 4;

pub(in crate::ocomp::retention) const RETAINED_EVIDENCE_WINDOW_BLOCKS: u64 = 64;

fn gc_ack_metadata_advanced(previous: PinRecordV1, current: PinRecordV1) -> bool {
    let mut expected = previous;
    let PinStateV1::GcPending {
        source_generation,
        export: Some(export),
        ..
    } = current.state
    else {
        return false;
    };
    if current.generation <= previous.generation || export.source_generation != source_generation {
        return false;
    }
    let PinStateV1::GcPending {
        export: slot @ None,
        ..
    } = &mut expected.state
    else {
        return false;
    };
    *slot = Some(export);
    expected.generation = current.generation;
    expected == current
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ocomp::retention) struct JobRegistryV1 {
    pub(in crate::ocomp::retention) generation: u64,
    pub(in crate::ocomp::retention) last_updated: B256,
    pub(in crate::ocomp::retention) records: BTreeMap<B256, PinRecordV1>,
}

pub(in crate::ocomp::retention) struct CoordinatorInner {
    pub(in crate::ocomp::retention) status: RetentionStatus,
    pub(in crate::ocomp::retention) registry: Option<JobRegistryV1>,
}

fn status_for_loaded_registry(
    registry: &Option<JobRegistryV1>,
) -> Result<RetentionStatus, RetentionError> {
    match registry {
        None => Ok(RetentionStatus::Empty),
        Some(registry) => registry
            .records
            .get(&registry.last_updated)
            .copied()
            .map(RetentionStatus::Ready)
            .ok_or(RetentionError::MalformedJournal(
                "registry last-updated key is missing",
            )),
    }
}

fn retention_status_kind(status: &RetentionStatus) -> &'static str {
    match status {
        RetentionStatus::Empty | RetentionStatus::Ready(_) => "available",
        RetentionStatus::Unavailable { .. } => "unavailable",
        RetentionStatus::Quarantined { .. } => "quarantined",
    }
}

fn publish_retention_status(previous: Option<&RetentionStatus>, status: &RetentionStatus) {
    let kind = retention_status_kind(status);
    metrics::gauge!("outbe_ocomp_retention_journal_available").set(if kind == "available" {
        1.0
    } else {
        0.0
    });
    metrics::gauge!("outbe_ocomp_retention_journal_unavailable").set(if kind == "unavailable" {
        1.0
    } else {
        0.0
    });
    metrics::gauge!("outbe_ocomp_retention_journal_quarantined").set(if kind == "quarantined" {
        1.0
    } else {
        0.0
    });

    if previous.map(retention_status_kind) == Some(kind) {
        return;
    }
    metrics::counter!(
        "outbe_ocomp_retention_journal_state_transitions_total",
        "to" => kind
    )
    .increment(1);
    match status {
        RetentionStatus::Unavailable {
            operation,
            path,
            reason,
        } => tracing::error!(
            operation,
            path = %path.display(),
            %reason,
            "OCOMP retention journal became unavailable; automatic recovery is active"
        ),
        RetentionStatus::Quarantined { reason } => tracing::error!(
            %reason,
            "OCOMP retention journal entered integrity quarantine; operator recovery is required"
        ),
        RetentionStatus::Empty | RetentionStatus::Ready(_) => {
            tracing::info!("OCOMP retention journal is available")
        }
    }
}

fn transition_retention_status(inner: &mut CoordinatorInner, status: RetentionStatus) {
    publish_retention_status(Some(&inner.status), &status);
    inner.status = status;
}

/// Node-owned independently keyed multi-job OCOMP pin coordinator.
pub struct OcompRetentionCoordinator {
    store: JournalStore,
    inner: Mutex<CoordinatorInner>,
    source: Arc<dyn FinalizedInputProofSource>,
    pub(in crate::ocomp::retention) retained_tributes: Option<Arc<RetainedTributeWriter>>,
    projection_fence: Option<Arc<ProjectionRetentionFence>>,
    pub(in crate::ocomp::retention) closure_checkpoint: AtomicU64,
}

impl OcompRetentionCoordinator {
    /// Open a managed journal root. Transient storage I/O enters recoverable
    /// fail-closed unavailability; corrupt or ambiguous authority is quarantined.
    pub fn open(root: impl Into<PathBuf>, source: Arc<dyn FinalizedInputProofSource>) -> Self {
        Self::open_with_durability(root, source, Arc::new(OsJournalDurability))
    }

    pub fn open_with_retained_tributes(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        retained_tributes: Arc<RetainedTributeWriter>,
    ) -> Self {
        Self::open_with_retained_tributes_and_fence(
            root,
            source,
            retained_tributes,
            Arc::new(ProjectionRetentionFence::default()),
        )
    }

    pub fn open_with_retained_tributes_and_fence(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        retained_tributes: Arc<RetainedTributeWriter>,
        projection_fence: Arc<ProjectionRetentionFence>,
    ) -> Self {
        Self::open_inner(
            root.into(),
            source,
            Arc::new(OsJournalDurability),
            Some(retained_tributes),
            Some(projection_fence),
        )
    }

    pub(crate) fn open_with_durability(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        durability: Arc<dyn JournalDurability>,
    ) -> Self {
        Self::open_inner(root.into(), source, durability, None, None)
    }

    pub(in crate::ocomp::retention) fn open_inner(
        root: PathBuf,
        source: Arc<dyn FinalizedInputProofSource>,
        durability: Arc<dyn JournalDurability>,
        retained_tributes: Option<Arc<RetainedTributeWriter>>,
        projection_fence: Option<Arc<ProjectionRetentionFence>>,
    ) -> Self {
        let store = JournalStore::new(root, durability);
        let (status, registry) = match store.initialize() {
            Ok(registry) => match status_for_loaded_registry(&registry) {
                Ok(status) => (status, registry),
                Err(error) => {
                    record_journal_failure(&error);
                    (status_for_journal_error(&error), registry)
                }
            },
            Err(error) => {
                record_journal_failure(&error);
                (status_for_journal_error(&error), None)
            }
        };
        publish_retention_status(None, &status);
        Self {
            store,
            inner: Mutex::new(CoordinatorInner { status, registry }),
            source,
            retained_tributes,
            projection_fence,
            closure_checkpoint: AtomicU64::new(0),
        }
    }

    pub fn status(&self) -> RetentionStatus {
        self.lock()
            .map(|inner| inner.status.clone())
            .unwrap_or_else(|error| RetentionStatus::Quarantined {
                reason: error.to_string(),
            })
    }

    pub(in crate::ocomp::retention) fn recover_journal(&self) -> Result<bool, RetentionError> {
        let previous_registry = {
            let inner = self.lock()?;
            if !matches!(inner.status, RetentionStatus::Unavailable { .. }) {
                return Ok(false);
            }
            inner.registry.clone()
        };

        let recovered = self.store.recover_and_load().and_then(|registry| {
            let status = status_for_loaded_registry(&registry)?;
            Ok((registry, status))
        });
        let mut inner = self.lock()?;
        if !matches!(inner.status, RetentionStatus::Unavailable { .. }) {
            return Ok(false);
        }
        match recovered {
            Ok((registry, status)) => {
                if let (Some(previous), Some(current)) =
                    (previous_registry.as_ref(), registry.as_ref())
                {
                    if previous != current && !journal_successor_is_exact(previous, current) {
                        let error = RetentionError::AmbiguousJournal(
                            "recovered authority is neither the current journal nor its exact successor",
                        );
                        transition_retention_status(&mut inner, status_for_journal_error(&error));
                        return Err(error);
                    }
                } else if previous_registry.is_some() && registry.is_none() {
                    let error = RetentionError::AmbiguousJournal(
                        "authoritative journal disappeared during in-process recovery",
                    );
                    transition_retention_status(&mut inner, status_for_journal_error(&error));
                    return Err(error);
                }
                inner.registry = registry;
                transition_retention_status(&mut inner, status);
                Ok(true)
            }
            Err(error) => {
                transition_retention_status(&mut inner, status_for_journal_error(&error));
                Err(error)
            }
        }
    }

    /// Returns every independently addressable finalized/exported job.
    pub fn finalized_live_jobs(&self) -> Result<Vec<FinalizedJobPinV1>, RetentionError> {
        let inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        let mut jobs = Vec::new();
        for record in inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.values())
        {
            match record.state {
                PinStateV1::Finalized {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                }
                | PinStateV1::Exported {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    ..
                } => jobs.push(FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                }),
                PinStateV1::Tentative { .. }
                | PinStateV1::Terminal { .. }
                | PinStateV1::GcPending { .. }
                | PinStateV1::OrphanGcPending { .. }
                | PinStateV1::Released { .. } => {}
            }
        }
        jobs.sort_by_key(|job| (job.candidate.block_number, job.candidate.block_hash));
        Ok(jobs)
    }

    pub fn finalized_job_record(
        &self,
        job_id: B256,
    ) -> Result<(u64, FinalizedJobPinV1), RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } => Ok((
                record.generation,
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
            )),
            _ => Err(RetentionError::InvalidTransition(
                "snapshot handoff requires the exact finalized Job",
            )),
        }
    }

    pub fn discovery_job_records(
        &self,
        job_id: B256,
    ) -> Result<Vec<(u64, FinalizedJobPinV1)>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        let (pin, generations) = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(record.generation), None],
            ),
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(export.source_generation), None],
            ),
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                ..
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(source_generation), None],
            ),
            PinStateV1::GcPending {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                ..
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(source_generation), None],
            ),
            PinStateV1::Tentative { .. }
            | PinStateV1::OrphanGcPending { .. }
            | PinStateV1::Released { .. } => {
                return Err(RetentionError::InvalidTransition(
                    "discovery requires a live or terminal finalized job",
                ));
            }
        };
        let generations = generations
            .into_iter()
            .flatten()
            .map(|generation| (generation, pin))
            .collect::<Vec<_>>();
        if generations.is_empty() {
            return Err(RetentionError::GenerationOverflow);
        }
        Ok(generations)
    }

    pub fn released_export_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ExportAuthorityV1>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(match record.state {
            PinStateV1::Released {
                job_id: Some(existing),
                reason: PinReleaseReason::RetentionSatisfied,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::Tentative { .. }
            | PinStateV1::Finalized { .. }
            | PinStateV1::Exported { .. }
            | PinStateV1::Terminal { .. }
            | PinStateV1::GcPending { .. }
            | PinStateV1::OrphanGcPending { .. }
            | PinStateV1::Released { .. } => None,
        })
    }

    pub fn released_job_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ReleasedJobAuthorityV1>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(match record.state {
            PinStateV1::Released {
                candidate,
                job_id: Some(existing),
                source_generation: Some(source_generation),
                reason: PinReleaseReason::RetentionSatisfied,
                export,
                ..
            } if existing == job_id => Some(ReleasedJobAuthorityV1 {
                candidate,
                job_id,
                source_generation,
                export,
            }),
            _ => None,
        })
    }

    pub fn prepare_candidate(&self, block: &ConsensusBlock) -> Result<(), OcompRetentionHookError> {
        let candidate = self.source.candidate_for_block(block).map_err(hook_error)?;
        if let Some(candidate) = candidate {
            self.record_tentative(candidate).map_err(hook_error)?;
        }
        Ok(())
    }

    pub fn reconcile_finalized(
        &self,
        block: &ConsensusBlock,
    ) -> Result<(), OcompRetentionHookError> {
        if let Some(error) = retention_status_error(&self.status()) {
            return Err(hook_error(error));
        }
        if let Some(candidate) = self.source.candidate_for_block(block).map_err(hook_error)? {
            self.record_tentative(candidate).map_err(hook_error)?;
        }
        let candidates = {
            let inner = self.lock().map_err(hook_error)?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Tentative { candidate }
                        if candidate.block_number <= block.number() =>
                    {
                        Some(candidate)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for candidate in candidates {
            match self
                .source
                .resolve_finality(candidate)
                .map_err(hook_error)?
            {
                CandidateFinalityV1::Finalized(finalized) => {
                    self.finalize_exact(finalized).map_err(hook_error)?;
                }
                CandidateFinalityV1::Orphaned => {
                    self.release_orphan(candidate, block.number())
                        .map_err(hook_error)?;
                }
            }
        }
        let live = {
            let inner = self.lock().map_err(hook_error)?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Finalized {
                        candidate, job_id, ..
                    }
                    | PinStateV1::Exported {
                        candidate, job_id, ..
                    } => Some((record.generation, candidate, job_id)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (generation, candidate, job_id) in live {
            if let Some(terminal_height) = self
                .source
                .terminal_height_at(block, candidate, job_id)
                .map_err(hook_error)?
            {
                let terminal_finality_height = terminal_height.max(block.number());
                self.observe_terminal(job_id, generation, terminal_finality_height)
                    .map_err(hook_error)?;
            }
        }
        Ok(())
    }

    /// Reconcile retention from the exact block and receipts owned by the
    /// unified finalized reader. This is the production finalized path; unlike
    /// [`Self::reconcile_finalized`], it performs no receipt-provider query.
    pub fn reconcile_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        observation: Option<FinalizedRequestObservationV1>,
    ) -> Result<(), RetentionError> {
        if let Some(error) = retention_status_error(&self.status()) {
            return Err(error);
        }
        let height = frame.identity().number;
        if let Some(observation) = observation {
            let candidate = self
                .source
                .candidate_for_finalized_observation(frame, observation)?;
            self.record_finalized_observation(candidate)?;
        }
        let live = {
            let inner = self.lock()?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Finalized {
                        candidate, job_id, ..
                    }
                    | PinStateV1::Exported {
                        candidate, job_id, ..
                    } => Some((record.generation, candidate, job_id)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (generation, candidate, job_id) in live {
            // A durable journal can be ahead of the replay cursor. Its later
            // jobs do not exist in this historical state yet.
            if candidate.block_number > height {
                continue;
            }
            if let Some(terminal_height) = self
                .source
                .terminal_height_at_finalized_frame(frame, candidate, job_id)?
            {
                self.observe_terminal(job_id, generation, terminal_height.max(height))?;
            }
        }
        Ok(())
    }

    /// Reobserving finalized history must preserve an already advanced lease.
    /// This does not relax speculative candidate admission or orphan handling.
    fn record_finalized_observation(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        if let Some(record) = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&candidate.block_hash))
            .copied()
        {
            if record_candidate(record) != candidate {
                return Err(RetentionError::ConflictingCandidate);
            }
            return match record.state {
                PinStateV1::OrphanGcPending { .. }
                | PinStateV1::Released {
                    reason: PinReleaseReason::Orphaned,
                    ..
                } => Err(RetentionError::OrphanedCandidate),
                _ => Ok(ack_for(record)),
            };
        }
        self.record_new_candidate_locked(&mut inner, candidate)
    }

    pub fn record_tentative(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        let key = candidate.block_hash;
        if let Some(record) = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            .copied()
        {
            return match record.state {
                PinStateV1::Tentative {
                    candidate: existing,
                } if existing == candidate => Ok(ack_for(record)),
                PinStateV1::Released {
                    candidate: existing,
                    reason: PinReleaseReason::Orphaned,
                    ..
                } if existing == candidate => Err(RetentionError::OrphanedCandidate),
                _ => Err(RetentionError::ConflictingCandidate),
            };
        }
        self.record_new_candidate_locked(&mut inner, candidate)
    }

    fn record_new_candidate_locked(
        &self,
        inner: &mut CoordinatorInner,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if inner.registry.as_ref().is_some_and(|registry| {
            registry.records.values().any(|record| {
                matches!(
                    record.state,
                    PinStateV1::GcPending { .. } | PinStateV1::OrphanGcPending { .. }
                ) && record_candidate(*record).input_lease_id == candidate.input_lease_id
            })
        }) {
            return Err(RetentionError::InvalidTransition(
                "input lease garbage collection is already in progress",
            ));
        }
        if inner.registry.as_ref().is_some_and(|registry| {
            registry
                .records
                .values()
                .filter(|record| !matches!(record.state, PinStateV1::Released { .. }))
                .count()
                >= JOURNAL_RECORD_COUNT_MAX
        }) {
            return Err(RetentionError::RegistryCapacity);
        }
        let generation = next_registry_generation(inner)?;
        self.persist_locked(
            inner,
            candidate.block_hash,
            PinRecordV1 {
                generation,
                state: PinStateV1::Tentative { candidate },
            },
        )
    }

    /// Finalizes one retained request exclusively from its canonical typed
    /// Metadosis record. The request event remains a locator; the chain record
    /// is the authority for JobId and every response-window height.
    pub fn bind_canonical_finalized_job(
        &self,
        candidate_block_hash: B256,
        record: &OcompJobRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let candidate = {
            let inner = self.lock()?;
            if let Some(error) = retention_status_error(&inner.status) {
                return Err(error);
            }
            inner
                .registry
                .as_ref()
                .and_then(|registry| registry.records.get(&candidate_block_hash))
                .copied()
                .map(record_candidate)
                .ok_or(RetentionError::InvalidTransition(
                    "canonical finalized job has no retained request candidate",
                ))?
        };
        if candidate.block_hash != candidate_block_hash {
            return Err(RetentionError::ConflictingCandidate);
        }
        self.finalize_exact(canonical_finalized_pin(candidate, record)?)
    }

    /// Replay can bind a historical candidate after the terminal frame was
    /// already observed. Apply that same canonical state without waiting for
    /// another block, and never move a retired record backwards.
    pub fn reconcile_canonical_terminal(
        &self,
        canonical: &OcompJobRecordV1,
        observed_height: u64,
    ) -> Result<(), RetentionError> {
        let finalized = canonical
            .finalized
            .as_ref()
            .ok_or(RetentionError::InvalidTransition(
                "canonical OCOMP job is not finalized",
            ))?;
        let record = {
            let inner = self.lock()?;
            let (_, record) = record_for_job(&inner, finalized.job_id)?;
            record
        };
        let candidate = record_candidate(record);
        canonical_finalized_pin(candidate, canonical)?;
        if matches!(
            record.state,
            PinStateV1::Finalized { .. } | PinStateV1::Exported { .. }
        ) {
            if let Some(height) = terminal_height_from_record(
                canonical,
                observed_height,
                candidate,
                finalized.job_id,
            )? {
                self.observe_terminal(finalized.job_id, record.generation, height)?;
            }
        }
        Ok(())
    }

    /// A crash may persist the spool ACK before retaining its export authority.
    /// Completing that metadata write never reactivates a retired lease. Expired
    /// jobs cannot adopt a late ACK, and speculative ACK admission stays strict.
    pub fn confirm_canonical_export_ack(
        &self,
        canonical: &OcompJobRecordV1,
        export: ExportAuthorityV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if canonical.status == OcompJobStatus::Expired {
            return Err(RetentionError::InvalidTransition(
                "expired job cannot adopt an export ACK",
            ));
        }
        let finalized = canonical
            .finalized
            .as_ref()
            .ok_or(RetentionError::InvalidTransition(
                "canonical OCOMP job is not finalized",
            ))?;
        {
            let inner = self.lock()?;
            let (_, record) = record_for_job(&inner, finalized.job_id)?;
            canonical_finalized_pin(record_candidate(record), canonical)?;
        }
        match self.confirm_export_ack(
            finalized.job_id,
            export.source_generation,
            export.lease_generation,
            export.manifest_hash,
        ) {
            Ok(ack) => return Ok(ack),
            Err(RetentionError::InvalidTransition(_)) => {}
            Err(error) => return Err(error),
        }
        if !matches!(
            canonical.status,
            OcompJobStatus::Completed | OcompJobStatus::Failed
        ) || export.source_generation == 0
            || export.lease_generation == 0
            || export.manifest_hash.is_zero()
        {
            return Err(RetentionError::InvalidTransition(
                "late export ACK requires exact terminal authority",
            ));
        }
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, finalized.job_id)?;
        canonical_finalized_pin(record_candidate(record), canonical)?;
        let mut state = record.state;
        let slot = match &mut state {
            PinStateV1::Terminal {
                source_generation,
                export: slot,
                ..
            }
            | PinStateV1::GcPending {
                source_generation,
                export: slot,
                ..
            } if *source_generation == export.source_generation => slot,
            PinStateV1::Released {
                source_generation: Some(source_generation),
                reason: PinReleaseReason::RetentionSatisfied,
                export: slot,
                ..
            } if *source_generation == export.source_generation => slot,
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "late export ACK has a different source generation",
                ))
            }
        };
        match *slot {
            Some(existing) if existing == export => return Ok(ack_for(record)),
            Some(_) => {
                return Err(RetentionError::InvalidTransition(
                    "late export ACK conflicts with retained authority",
                ))
            }
            None => *slot = Some(export),
        }
        self.persist_next(&mut inner, key, record, state)
    }

    pub fn record_exported(
        &self,
        job_id: B256,
        expected_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, job_id)?;
        if exact_export_replay(
            record,
            job_id,
            expected_generation,
            lease_generation,
            manifest_hash,
        ) {
            return Ok(ack_for(record));
        }
        ensure_generation(record, expected_generation)?;
        let (candidate, finality_recorded_height, open_height, deadline_height) = match record.state
        {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
            ),
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "export requires the exact finalized job",
                ));
            }
        };
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export: ExportAuthorityV1 {
                    source_generation: expected_generation,
                    lease_generation,
                    manifest_hash,
                },
            },
        )
    }

    /// Confirm a durable spool ACK across restart windows. A live finalized
    /// record performs the Exported transition; later states accept only the
    /// exact source generation that must have preceded them.
    pub fn confirm_export_ack(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        if source_generation == 0 || lease_generation == 0 || manifest_hash.is_zero() {
            return Err(RetentionError::InvalidTransition(
                "export ACK authority is incomplete",
            ));
        }
        match self.record_exported(job_id, source_generation, lease_generation, manifest_hash) {
            Ok(ack) => return Ok(ack),
            Err(RetentionError::InvalidTransition(_) | RetentionError::StaleGeneration { .. }) => {}
            Err(error) => return Err(error),
        }
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        let export = match record.state {
            PinStateV1::Terminal {
                job_id: existing,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::GcPending {
                job_id: existing,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::Released {
                job_id: Some(existing),
                reason: PinReleaseReason::RetentionSatisfied,
                export,
                ..
            } if existing == job_id => export,
            _ => None,
        };
        if export
            == Some(ExportAuthorityV1 {
                source_generation,
                lease_generation,
                manifest_hash,
            })
        {
            Ok(ack_for(record))
        } else {
            Err(RetentionError::InvalidTransition(
                "export ACK does not match the retained source generation",
            ))
        }
    }

    pub fn replay_exported(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<Option<DurablePinAck>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(exact_export_replay(
            record,
            job_id,
            source_generation,
            lease_generation,
            manifest_hash,
        )
        .then(|| ack_for(record)))
    }

    pub fn build_finalized_intent_proof(
        &self,
        job_id: B256,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        let candidate = self.live_candidate(job_id)?;
        let proof = self.source.build_finalized_intent_proof(candidate)?;
        let limits = poc_schema_limits();
        let intent = proof
            .decoded_intent(&limits)
            .map_err(|error| RetentionError::Source(format!("decode finalized intent: {error}")))?;
        let proof_intent_id = intent
            .intent_id(&limits)
            .map_err(|error| RetentionError::Source(format!("derive proof IntentId: {error}")))?;
        let proof_job_id = intent
            .job_id(candidate.block_hash, candidate.state_root, &limits)
            .map_err(|error| RetentionError::Source(format!("derive proof JobId: {error}")))?;
        if proof_job_id != job_id
            || proof_intent_id != candidate.intent_id
            || proof.protocol_bundle_hash != candidate.protocol_bundle_hash
            || proof.parent_accounting.finalized_block_number != candidate.block_number
            || proof.parent_accounting.finalized_block_hash != candidate.block_hash
            || intent.wwd != candidate.wwd
            || intent.ce_sealed_root != candidate.ce_sealed_root
            || intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?
                != candidate.input_lease_id
        {
            return Err(RetentionError::Source(
                "finalized-intent proof differs from the exact live pin".to_owned(),
            ));
        }
        Ok(proof)
    }

    pub fn build_lysis_openings(
        &self,
        job_id: B256,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        let candidate = self.live_candidate(job_id)?;
        let proof = self.source.build_lysis_openings(candidate, subjects)?;
        if proof.job_id != job_id
            || proof.protocol_bundle_hash != candidate.protocol_bundle_hash
            || proof.finalized_block_hash != candidate.block_hash
            || proof.finalized_state_root != candidate.state_root
            || proof.wwd != candidate.wwd
        {
            return Err(RetentionError::Source(
                "Lysis openings differ from the exact live pin".to_owned(),
            ));
        }
        Ok(proof)
    }

    pub fn observe_terminal(
        &self,
        job_id: B256,
        expected_generation: u64,
        terminal_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        let release_height = terminal_height
            .checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS)
            .ok_or(RetentionError::InvalidTransition(
                "terminal release height overflows",
            ))?;
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, job_id)?;
        ensure_generation(record, expected_generation)?;
        let (
            candidate,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
        ) = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
                record.generation,
                None,
            ),
            PinStateV1::Exported {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
                export.source_generation,
                Some(export),
            ),
            PinStateV1::Terminal {
                job_id: existing,
                terminal_height: existing_terminal,
                release_height: existing_release,
                ..
            } if existing == job_id
                && existing_terminal == terminal_height
                && existing_release == release_height =>
            {
                return Ok(ack_for(record));
            }
            PinStateV1::GcPending {
                job_id: existing,
                terminal_height: existing_terminal,
                release_height: existing_release,
                ..
            } if existing == job_id
                && existing_terminal == terminal_height
                && existing_release == release_height =>
            {
                return Ok(ack_for(record));
            }
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "terminal transition requires the exact live job",
                ));
            }
        };
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                export,
                terminal_height,
                release_height,
            },
        )
    }

    pub fn release_due(
        &self,
        finalized_height: u64,
    ) -> Result<Option<DurablePinAck>, RetentionError> {
        for work in self.gc_candidate_work(finalized_height)? {
            match self.release_due_work(work, finalized_height) {
                Ok(RetainedGcAttemptOutcome::Completed(ack)) => return Ok(Some(ack)),
                Ok(
                    RetainedGcAttemptOutcome::PageProgress
                    | RetainedGcAttemptOutcome::NoLongerPending,
                ) => {}
                Err(error) => return Err(error.into_error()),
            }
        }
        Ok(None)
    }

    pub(in crate::ocomp::retention) fn gc_candidate_work(
        &self,
        finalized_height: u64,
    ) -> Result<Vec<RetainedGcWorkId>, RetentionError> {
        let inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        Ok(inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.iter())
            .filter_map(|(key, record)| match record.state {
                PinStateV1::Terminal { release_height, .. }
                    if release_height <= finalized_height =>
                {
                    Some(RetainedGcWorkId {
                        key: *key,
                        generation: record.generation,
                    })
                }
                PinStateV1::GcPending { .. } | PinStateV1::OrphanGcPending { .. } => {
                    Some(RetainedGcWorkId {
                        key: *key,
                        generation: record.generation,
                    })
                }
                _ => None,
            })
            .collect())
    }

    pub(in crate::ocomp::retention) fn release_due_work(
        &self,
        work: RetainedGcWorkId,
        finalized_height: u64,
    ) -> Result<RetainedGcAttemptOutcome, RetainedGcAttemptFailure> {
        let key = work.key;
        let projection_fence = self.projection_fence.clone();
        let _projection_guard = projection_fence
            .as_ref()
            .map(|fence| {
                fence
                    .gc_claim_guard()
                    .map_err(RetentionError::InvalidTransition)
            })
            .transpose()
            .map_err(RetainedGcAttemptFailure::global)?;
        let mut inner = self.lock().map_err(RetainedGcAttemptFailure::global)?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(RetainedGcAttemptFailure::global(error));
        }
        let Some(record) = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            .copied()
        else {
            return Ok(RetainedGcAttemptOutcome::NoLongerPending);
        };
        if record.generation != work.generation {
            return Ok(RetainedGcAttemptOutcome::NoLongerPending);
        }
        let gc_record = match record.state {
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                source_generation,
                export,
                terminal_height,
                release_height,
            } if release_height <= finalized_height => {
                if lease_has_other_references(&inner, key, candidate.input_lease_id)
                    || self.retained_tributes.is_none()
                {
                    return self
                        .persist_next(
                            &mut inner,
                            key,
                            record,
                            PinStateV1::Released {
                                candidate,
                                job_id: Some(job_id),
                                source_generation: Some(source_generation),
                                reason: PinReleaseReason::RetentionSatisfied,
                                observed_height: finalized_height,
                                export,
                            },
                        )
                        .map(RetainedGcAttemptOutcome::Completed)
                        .map_err(RetainedGcAttemptFailure::global);
                }
                let ack = self
                    .persist_next(
                        &mut inner,
                        key,
                        record,
                        PinStateV1::GcPending {
                            candidate,
                            job_id,
                            finality_recorded_height,
                            open_height,
                            deadline_height,
                            source_generation,
                            export,
                            terminal_height,
                            release_height,
                        },
                    )
                    .map_err(RetainedGcAttemptFailure::global)?;
                inner
                    .registry
                    .as_ref()
                    .and_then(|registry| registry.records.get(&key))
                    .copied()
                    .filter(|current| current.generation == ack.generation)
                    .ok_or(RetentionError::InvalidTransition(
                        "GC claim disappeared after durable publication",
                    ))
                    .map_err(RetainedGcAttemptFailure::global)?
            }
            PinStateV1::GcPending { .. } | PinStateV1::OrphanGcPending { .. } => record,
            _ => return Ok(RetainedGcAttemptOutcome::NoLongerPending),
        };
        drop(inner);
        drop(_projection_guard);

        let (candidate, completed_state) = match gc_record.state {
            PinStateV1::GcPending {
                candidate,
                job_id,
                source_generation,
                export,
                ..
            } => (
                candidate,
                PinStateV1::Released {
                    candidate,
                    job_id: Some(job_id),
                    source_generation: Some(source_generation),
                    reason: PinReleaseReason::RetentionSatisfied,
                    observed_height: finalized_height,
                    export,
                },
            ),
            PinStateV1::OrphanGcPending {
                candidate,
                observed_height,
            } => (
                candidate,
                PinStateV1::Released {
                    candidate,
                    job_id: None,
                    source_generation: None,
                    reason: PinReleaseReason::Orphaned,
                    observed_height,
                    export: None,
                },
            ),
            _ => unreachable!("retained GC work is durably claimed before MongoDB I/O"),
        };
        let complete = self
            .retained_tributes
            .as_ref()
            .expect("GcPending is unreachable without retained Tribute storage")
            .release_input_lease_page(candidate.input_lease_id)
            .map_err(|error| classify_retained_gc_failure(key, gc_record.generation, error))?;
        if !complete {
            return Ok(RetainedGcAttemptOutcome::PageProgress);
        }

        let mut inner = self.lock().map_err(RetainedGcAttemptFailure::global)?;
        let current = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            .copied()
            .ok_or(RetentionError::InvalidTransition(
                "GC claim disappeared before completion",
            ))
            .map_err(RetainedGcAttemptFailure::global)?;
        if current != gc_record {
            // A canonical ACK can be durably attached while GC is doing Mongo
            // I/O outside this lock. Recheck deletion under its new generation
            // instead of publishing Released with the old ACK-less metadata.
            if gc_ack_metadata_advanced(gc_record, current) {
                return Ok(RetainedGcAttemptOutcome::NoLongerPending);
            }
            return Err(RetainedGcAttemptFailure::global(
                RetentionError::InvalidTransition("GC claim changed before completion"),
            ));
        }
        self.persist_next(&mut inner, key, current, completed_state)
            .map(RetainedGcAttemptOutcome::Completed)
            .map_err(RetainedGcAttemptFailure::global)
    }

    pub fn is_signable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        record_for_job(&inner, job_id).is_ok_and(|(_, record)| {
            matches!(
                record.state,
                PinStateV1::Exported {
                    job_id: current, ..
                } if current == job_id
            )
        })
    }

    pub fn is_exportable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        record_for_job(&inner, job_id).is_ok_and(|(_, record)| {
            matches!(
                record.state,
                PinStateV1::Finalized {
                    job_id: current, ..
                } | PinStateV1::Exported {
                    job_id: current, ..
                } if current == job_id
            )
        })
    }

    fn live_candidate(&self, job_id: B256) -> Result<CandidatePinV1, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: current,
                ..
            }
            | PinStateV1::Exported {
                candidate,
                job_id: current,
                ..
            } if current == job_id => Ok(candidate),
            _ => Err(RetentionError::InvalidTransition(
                "proof construction requires the exact live finalized job",
            )),
        }
    }

    fn finalize_exact(
        &self,
        finalized: FinalizedJobPinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let key = finalized.candidate.block_hash;
        let record = record_for_candidate(&inner, finalized.candidate)?;
        match record.state {
            PinStateV1::Tentative { candidate } if candidate == finalized.candidate => self
                .persist_next(
                    &mut inner,
                    key,
                    record,
                    PinStateV1::Finalized {
                        candidate,
                        job_id: finalized.job_id,
                        finality_recorded_height: finalized.finality_recorded_height,
                        open_height: finalized.open_height,
                        deadline_height: finalized.deadline_height,
                    },
                ),
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if candidate == finalized.candidate
                && job_id == finalized.job_id
                && finality_recorded_height == finalized.finality_recorded_height
                && open_height == finalized.open_height
                && deadline_height == finalized.deadline_height =>
            {
                Ok(ack_for(record))
            }
            PinStateV1::Released { candidate, .. } if candidate == finalized.candidate => {
                Err(RetentionError::OrphanedCandidate)
            }
            _ => Err(RetentionError::InvalidTransition(
                "finality does not match the tentative candidate",
            )),
        }
    }

    fn release_orphan(
        &self,
        candidate: CandidatePinV1,
        observed_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        let projection_fence = self.projection_fence.clone();
        let _projection_guard = projection_fence
            .as_ref()
            .map(|fence| {
                fence
                    .gc_claim_guard()
                    .map_err(RetentionError::InvalidTransition)
            })
            .transpose()?;
        let mut inner = self.lock()?;
        let key = candidate.block_hash;
        let record = record_for_candidate(&inner, candidate)?;
        match record.state {
            PinStateV1::Tentative { candidate: current } if current == candidate => {
                self.release_orphan_locked(&mut inner, key, record, candidate, observed_height)
            }
            PinStateV1::Released {
                candidate: current,
                reason: PinReleaseReason::Orphaned,
                ..
            } if current == candidate => Ok(ack_for(record)),
            _ => Err(RetentionError::InvalidTransition(
                "orphan release does not match the tentative candidate",
            )),
        }
    }

    fn release_orphan_locked(
        &self,
        inner: &mut CoordinatorInner,
        key: B256,
        record: PinRecordV1,
        candidate: CandidatePinV1,
        observed_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        let state = if lease_has_other_references(inner, key, candidate.input_lease_id)
            || self.retained_tributes.is_none()
        {
            PinStateV1::Released {
                candidate,
                job_id: None,
                source_generation: None,
                reason: PinReleaseReason::Orphaned,
                observed_height,
                export: None,
            }
        } else {
            PinStateV1::OrphanGcPending {
                candidate,
                observed_height,
            }
        };
        self.persist_next(inner, key, record, state)
    }

    fn persist_next(
        &self,
        inner: &mut CoordinatorInner,
        key: B256,
        current: PinRecordV1,
        state: PinStateV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            != Some(&current)
        {
            return Err(RetentionError::InvalidTransition(
                "Job Registry entry changed before transition",
            ));
        }
        let generation = next_registry_generation(inner)?;
        self.persist_locked(inner, key, PinRecordV1 { generation, state })
    }

    fn persist_locked(
        &self,
        inner: &mut CoordinatorInner,
        key: B256,
        record: PinRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        let mut registry = inner.registry.clone().unwrap_or_else(|| JobRegistryV1 {
            generation: record.generation,
            last_updated: key,
            records: BTreeMap::new(),
        });
        if !registry.records.contains_key(&key)
            && registry.records.len() >= JOURNAL_RECORD_PRESSURE_WATERMARK
        {
            let closure_checkpoint = self.closure_checkpoint.load(Ordering::Acquire);
            registry.records.retain(|_, existing| {
                !matches!(existing.state, PinStateV1::Released { .. })
                    || record_candidate(*existing).block_number > closure_checkpoint
            });
        }
        if !registry.records.contains_key(&key)
            && registry.records.len() >= JOURNAL_RECORD_COUNT_MAX
        {
            return Err(RetentionError::RegistryCapacity);
        }
        registry.generation = record.generation;
        registry.last_updated = key;
        registry.records.insert(key, record);
        match self.store.persist(&registry, record) {
            Ok(ack) => {
                inner.registry = Some(registry);
                transition_retention_status(inner, RetentionStatus::Ready(record));
                Ok(ack)
            }
            Err(error) => {
                record_journal_failure(&error);
                transition_retention_status(inner, status_for_journal_error(&error));
                Err(error)
            }
        }
    }

    pub(in crate::ocomp::retention) fn lock(
        &self,
    ) -> Result<MutexGuard<'_, CoordinatorInner>, RetentionError> {
        self.inner.lock().map_err(|_| RetentionError::Poisoned)
    }
}

fn next_registry_generation(inner: &CoordinatorInner) -> Result<u64, RetentionError> {
    inner
        .registry
        .as_ref()
        .map_or(0, |registry| registry.generation)
        .checked_add(1)
        .ok_or(RetentionError::GenerationOverflow)
}

fn record_for_candidate(
    inner: &CoordinatorInner,
    candidate: CandidatePinV1,
) -> Result<PinRecordV1, RetentionError> {
    if let Some(error) = retention_status_error(&inner.status) {
        return Err(error);
    }
    inner
        .registry
        .as_ref()
        .and_then(|registry| registry.records.get(&candidate.block_hash))
        .copied()
        .filter(|record| record_candidate(*record) == candidate)
        .ok_or(RetentionError::InvalidTransition(
            "candidate has no exact Job Registry entry",
        ))
}

pub(in crate::ocomp::retention) fn record_for_job(
    inner: &CoordinatorInner,
    job_id: B256,
) -> Result<(B256, PinRecordV1), RetentionError> {
    if let Some(error) = retention_status_error(&inner.status) {
        return Err(error);
    }
    inner
        .registry
        .as_ref()
        .into_iter()
        .flat_map(|registry| registry.records.iter())
        .find_map(|(key, record)| {
            matches!(
                record.state,
                PinStateV1::Finalized {
                    job_id: current, ..
                } | PinStateV1::Exported {
                    job_id: current, ..
                } | PinStateV1::Terminal {
                    job_id: current, ..
                } | PinStateV1::GcPending {
                    job_id: current, ..
                } | PinStateV1::Released {
                    job_id: Some(current),
                    ..
                } if current == job_id
            )
            .then_some((*key, *record))
        })
        .ok_or(RetentionError::InvalidTransition(
            "JobId has no exact Job Registry entry",
        ))
}

fn lease_has_other_references(
    inner: &CoordinatorInner,
    excluded_key: B256,
    input_lease_id: B256,
) -> bool {
    inner.registry.as_ref().is_some_and(|registry| {
        registry.records.iter().any(|(key, record)| {
            *key != excluded_key
                && record_candidate(*record).input_lease_id == input_lease_id
                && !matches!(record.state, PinStateV1::Released { .. })
        })
    })
}

pub(in crate::ocomp::retention) const fn record_candidate(record: PinRecordV1) -> CandidatePinV1 {
    match record.state {
        PinStateV1::Tentative { candidate }
        | PinStateV1::Finalized { candidate, .. }
        | PinStateV1::Exported { candidate, .. }
        | PinStateV1::Terminal { candidate, .. }
        | PinStateV1::GcPending { candidate, .. }
        | PinStateV1::OrphanGcPending { candidate, .. }
        | PinStateV1::Released { candidate, .. } => candidate,
    }
}

fn ensure_generation(record: PinRecordV1, expected_generation: u64) -> Result<(), RetentionError> {
    if record.generation != expected_generation {
        return Err(RetentionError::StaleGeneration {
            expected: expected_generation,
            actual: record.generation,
        });
    }
    Ok(())
}

pub(in crate::ocomp::retention) fn ack_for(record: PinRecordV1) -> DurablePinAck {
    DurablePinAck {
        generation: record.generation,
        record_hash: keccak256(encode_record(record)),
    }
}

fn exact_export_replay(
    record: PinRecordV1,
    job_id: B256,
    source_generation: u64,
    lease_generation: u64,
    manifest_hash: B256,
) -> bool {
    matches!(
        record.state,
        PinStateV1::Exported {
            job_id: existing,
            export,
            ..
        } if existing == job_id
            && export == ExportAuthorityV1 {
                source_generation,
                lease_generation,
                manifest_hash,
            }
    )
}

pub(in crate::ocomp::retention) fn candidate_job_id(
    candidate: CandidatePinV1,
) -> Result<B256, RetentionError> {
    job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .map_err(|error| RetentionError::Source(format!("derive tentative JobId: {error}")))
}
