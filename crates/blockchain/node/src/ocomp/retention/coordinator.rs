use crate::ocomp::retention::*;

pub(in crate::ocomp::retention) const JOURNAL_RECORD_PRESSURE_WATERMARK: usize =
    JOURNAL_RECORD_COUNT_MAX - JOURNAL_RECORD_COUNT_MAX / 4;

pub(in crate::ocomp::retention) const RETAINED_EVIDENCE_WINDOW_BLOCKS: u64 = 64;

pub(in crate::ocomp::retention) fn gc_ack_metadata_advanced(
    previous: PinRecordV1,
    current: PinRecordV1,
) -> bool {
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

pub(in crate::ocomp::retention) fn status_for_loaded_registry(
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

pub(in crate::ocomp::retention) fn retention_status_kind(status: &RetentionStatus) -> &'static str {
    match status {
        RetentionStatus::Empty | RetentionStatus::Ready(_) => "available",
        RetentionStatus::Unavailable { .. } => "unavailable",
        RetentionStatus::Quarantined { .. } => "quarantined",
    }
}

pub(in crate::ocomp::retention) fn publish_retention_status(
    previous: Option<&RetentionStatus>,
    status: &RetentionStatus,
) {
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

pub(in crate::ocomp::retention) fn transition_retention_status(
    inner: &mut CoordinatorInner,
    status: RetentionStatus,
) {
    publish_retention_status(Some(&inner.status), &status);
    inner.status = status;
}

/// Node-owned independently keyed multi-job OCOMP pin coordinator.
pub struct OcompRetentionCoordinator {
    pub(in crate::ocomp::retention) store: JournalStore,
    pub(in crate::ocomp::retention) inner: Mutex<CoordinatorInner>,
    pub(in crate::ocomp::retention) source: Arc<dyn FinalizedInputProofSource>,
    pub(in crate::ocomp::retention) retained_tributes: Option<Arc<RetainedTributeWriter>>,
    pub(in crate::ocomp::retention) projection_fence: Option<Arc<ProjectionRetentionFence>>,
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

    pub(in crate::ocomp::retention) fn persist_next(
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

    pub(in crate::ocomp::retention) fn persist_locked(
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

pub(in crate::ocomp::retention) fn next_registry_generation(
    inner: &CoordinatorInner,
) -> Result<u64, RetentionError> {
    inner
        .registry
        .as_ref()
        .map_or(0, |registry| registry.generation)
        .checked_add(1)
        .ok_or(RetentionError::GenerationOverflow)
}

pub(in crate::ocomp::retention) fn record_for_candidate(
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
                    job_id: current,
                    ..
                } if current == job_id
            )
            .then_some((*key, *record))
        })
        .ok_or(RetentionError::InvalidTransition(
            "JobId has no exact Job Registry entry",
        ))
}

pub(in crate::ocomp::retention) fn lease_has_other_references(
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
        PinStateV1::AwaitingJobFinalization { candidate }
        | PinStateV1::Finalized { candidate, .. }
        | PinStateV1::Exported { candidate, .. }
        | PinStateV1::Terminal { candidate, .. }
        | PinStateV1::GcPending { candidate, .. }
        | PinStateV1::Released { candidate, .. } => candidate,
    }
}

pub(in crate::ocomp::retention) fn ensure_generation(
    record: PinRecordV1,
    expected_generation: u64,
) -> Result<(), RetentionError> {
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

pub(in crate::ocomp::retention) fn exact_export_replay(
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
    .map_err(|error| RetentionError::Source(format!("derive finalized request JobId: {error}")))
}
