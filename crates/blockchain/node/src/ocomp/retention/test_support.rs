use super::{
    coordinator::{record_for_job, JOURNAL_RECORD_PRESSURE_WATERMARK},
    gc::atomic_max,
};
use crate::ocomp::retention::*;

#[cfg(test)]
pub(crate) struct RetainedGcCycleTestReport {
    pub global_deferred: bool,
    pub pending: usize,
    pub deferred: usize,
    pub completed: u64,
    pub pages: u64,
    pub item_failures: usize,
    pub retry_entries: usize,
}

#[cfg(test)]
const FINALITY_NOTIFICATION_CAPACITY: usize = 256;

#[cfg(test)]
const FINALITY_RECONCILIATION_INITIAL_BACKOFF_MS: u64 = 25;

#[cfg(test)]
const FINALITY_RECONCILIATION_MAX_BACKOFF_MS: u64 = 800;

/// Bounded, ordered node-owned worker for finalized-block reconciliation.
///
/// Before a tentative pin exists, every finalized block is an input-discovery
/// boundary and must remain observable: a later block cannot stand in for the
/// request receipts of an earlier block. The bounded FIFO therefore preserves
/// exact notification order and rejects overflow instead of silently
/// coalescing away a possible request.
#[cfg(test)]
pub struct OcompRetentionService {
    coordinator: Arc<OcompRetentionCoordinator>,
    snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
    finalized_rx: tokio::sync::mpsc::Receiver<ConsensusBlock>,
    execution_ready_rx: tokio::sync::watch::Receiver<u64>,
}

/// Arms the exact finalized snapshot before later finalized blocks can advance
/// the CE marker. This callback runs in the node-owned retention worker, never
/// in the consensus finalization actor.
#[cfg(test)]
pub trait FinalizedSnapshotArmer: Send + Sync {
    fn arm_finalized_snapshot(&self, job_id: B256) -> Result<(), String>;
}

/// Executor-facing notification that exact canonical receipts through `height`
/// are locally readable. It carries no consensus authority: certified block
/// identity still comes exclusively from [`OcompRetentionHandle`].
#[derive(Clone)]
#[cfg(test)]
pub struct OcompRetentionExecutionHandle {
    execution_ready_tx: tokio::sync::watch::Sender<u64>,
}

#[cfg(test)]
impl OcompRetentionService {
    pub fn new(coordinator: Arc<OcompRetentionCoordinator>) -> (Self, OcompRetentionHandle) {
        Self::new_with_snapshot_armer(coordinator, None)
    }

    pub fn new_with_snapshot_armer(
        coordinator: Arc<OcompRetentionCoordinator>,
        snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
    ) -> (Self, OcompRetentionHandle) {
        let (execution_ready_tx, execution_ready_rx) = tokio::sync::watch::channel(u64::MAX);
        drop(execution_ready_tx);
        Self::new_inner(coordinator, snapshot_armer, execution_ready_rx)
    }

    /// Construct the production ordered join between certified finality and
    /// exact local execution. `initial_execution_ready_height` is the executor
    /// recovery anchor; later progress is supplied through the returned handle.
    pub fn new_with_execution_readiness(
        coordinator: Arc<OcompRetentionCoordinator>,
        snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
        initial_execution_ready_height: u64,
    ) -> (Self, OcompRetentionHandle, OcompRetentionExecutionHandle) {
        let (execution_ready_tx, execution_ready_rx) =
            tokio::sync::watch::channel(initial_execution_ready_height);
        let (service, handle) = Self::new_inner(coordinator, snapshot_armer, execution_ready_rx);
        (
            service,
            handle,
            OcompRetentionExecutionHandle { execution_ready_tx },
        )
    }

    fn new_inner(
        coordinator: Arc<OcompRetentionCoordinator>,
        snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
        execution_ready_rx: tokio::sync::watch::Receiver<u64>,
    ) -> (Self, OcompRetentionHandle) {
        let (finalized_tx, finalized_rx) =
            tokio::sync::mpsc::channel(FINALITY_NOTIFICATION_CAPACITY);
        (
            Self {
                coordinator: coordinator.clone(),
                snapshot_armer,
                finalized_rx,
                execution_ready_rx,
            },
            OcompRetentionHandle {
                coordinator,
                finalized_tx: Some(finalized_tx),
            },
        )
    }

    pub async fn run(mut self) {
        while let Some(block) = self.finalized_rx.recv().await {
            let block_number = block.number();
            let block_hash = block.block_hash();
            while block_number > *self.execution_ready_rx.borrow_and_update() {
                if self.execution_ready_rx.changed().await.is_err() {
                    tracing::warn!(
                        target: "outbe::ocomp",
                        block_number,
                        block_hash = %block_hash,
                        execution_ready_height = *self.execution_ready_rx.borrow(),
                        "OCOMP execution-readiness source closed with finalized work pending; marshal replay will recover it after restart"
                    );
                    return;
                }
            }

            let mut attempt = 0_u64;
            loop {
                let coordinator = self.coordinator.clone();
                let snapshot_armer = self.snapshot_armer.clone();
                let finalized_block = block.clone();
                match tokio::task::spawn_blocking(move || {
                    coordinator
                        .reconcile_finalized(&finalized_block)
                        .map_err(|error| error.to_string())?;
                    if let Some(snapshot_armer) = snapshot_armer {
                        for job in coordinator
                            .finalized_live_jobs()
                            .map_err(|error| error.to_string())?
                        {
                            snapshot_armer.arm_finalized_snapshot(job.job_id)?;
                        }
                    }
                    Ok::<(), String>(())
                })
                .await
                {
                    Ok(Ok(())) => break,
                    Ok(Err(error)) => {
                        attempt = attempt.saturating_add(1);
                        let backoff_ms = FINALITY_RECONCILIATION_INITIAL_BACKOFF_MS
                            .saturating_mul(1_u64 << attempt.saturating_sub(1).min(5))
                            .min(FINALITY_RECONCILIATION_MAX_BACKOFF_MS);
                        if attempt == 1 || attempt.is_multiple_of(64) {
                            tracing::warn!(
                                target: "outbe::ocomp",
                                block_number,
                                block_hash = %block_hash,
                                attempt,
                                backoff_ms,
                                %error,
                                "OCOMP pin reconciliation is stalled; retaining the exact finalized block"
                            );
                        } else {
                            tracing::debug!(
                                target: "outbe::ocomp",
                                block_number,
                                block_hash = %block_hash,
                                attempt,
                                backoff_ms,
                                %error,
                                "OCOMP pin reconciliation will retry the same exact finalized block"
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    }
                    Err(error) => {
                        attempt = attempt.saturating_add(1);
                        tracing::warn!(
                            target: "outbe::ocomp",
                            block_number,
                            block_hash = %block_hash,
                            attempt,
                            %error,
                            "OCOMP pin reconciliation task failed; retaining the exact finalized block"
                        );
                        tokio::time::sleep(Duration::from_millis(
                            FINALITY_RECONCILIATION_MAX_BACKOFF_MS,
                        ))
                        .await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
impl OcompRetentionExecutionHandle {
    pub fn notify_execution_finalized(&self, height: u64) -> Result<(), OcompRetentionHookError> {
        if self.execution_ready_tx.receiver_count() == 0 {
            return Err(OcompRetentionHookError::new(
                "OCOMP retention execution-readiness worker is unavailable",
            ));
        }
        self.execution_ready_tx.send_if_modified(|current| {
            if height > *current {
                *current = height;
                true
            } else {
                false
            }
        });
        Ok(())
    }
}

#[cfg(test)]
pub(crate) const fn retention_pressure_watermark_for_test() -> usize {
    JOURNAL_RECORD_PRESSURE_WATERMARK
}

#[cfg(test)]
pub(crate) fn seed_retention_journal_for_test(
    root: impl AsRef<Path>,
    generation: u64,
    last_updated: B256,
    records: Vec<(B256, PinRecordV1)>,
) -> Result<(), RetentionError> {
    let record_count = records.len();
    let records = records.into_iter().collect::<BTreeMap<_, _>>();
    if records.is_empty()
        || records.len() != record_count
        || records.len() > JOURNAL_RECORD_COUNT_MAX
        || !records.contains_key(&last_updated)
        || records.values().map(|record| record.generation).max() != Some(generation)
    {
        return Err(RetentionError::MalformedJournal(
            "invalid canonical test seed registry",
        ));
    }
    let changed = *records
        .get(&last_updated)
        .expect("validated last-updated test record");
    let registry = JobRegistryV1 {
        generation,
        last_updated,
        records,
    };
    let store = JournalStore::new(root.as_ref().to_path_buf(), Arc::new(OsJournalDurability));
    if store.initialize()?.is_some() {
        return Err(RetentionError::InvalidTransition(
            "test seed journal already exists",
        ));
    }
    store.persist(&registry, changed)?;
    Ok(())
}

impl OcompRetentionCoordinator {
    #[cfg(test)]
    pub(crate) fn open_with_retained_tributes_and_durability(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        retained_tributes: Arc<RetainedTributeWriter>,
        durability: Arc<dyn JournalDurability>,
    ) -> Self {
        Self::open_inner(
            root.into(),
            source,
            durability,
            Some(retained_tributes),
            Some(Arc::new(ProjectionRetentionFence::default())),
        )
    }

    #[cfg(test)]
    pub(crate) fn set_closure_checkpoint_for_test(&self, height: u64) {
        atomic_max(&self.closure_checkpoint, height);
    }

    /// Returns the exact live `Exported` record addressed by `JobId`.
    ///
    /// This deliberately consults the durable multi-job registry rather than
    /// [`Self::status`], whose single operational summary may describe a newer
    /// job. Callers must not substitute another live job.
    #[cfg(test)]
    pub(crate) fn exported_job_record(&self, job_id: B256) -> Result<PinRecordV1, RetentionError> {
        let inner = self.lock()?;
        if let Some(error) = retention_status_error(&inner.status) {
            return Err(error);
        }
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Exported {
                job_id: current, ..
            } if current == job_id => Ok(record),
            _ => Err(RetentionError::InvalidTransition(
                "attestation requires the exact exported Job",
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn run_gc_cycle_with_retry_for_test(
        &self,
        finalized_height: u64,
        now: Instant,
        retry_schedule: &mut RetainedGcRetrySchedule,
    ) -> Result<crate::ocomp::retention::RetainedGcCycleTestReport, RetentionError> {
        self.run_gc_cycle_with_retry_clock_for_test(finalized_height, now, now, retry_schedule)
    }

    #[cfg(test)]
    pub(crate) fn run_gc_cycle_with_retry_clock_for_test(
        &self,
        finalized_height: u64,
        eligibility_now: Instant,
        retry_now: Instant,
        retry_schedule: &mut RetainedGcRetrySchedule,
    ) -> Result<crate::ocomp::retention::RetainedGcCycleTestReport, RetentionError> {
        match self
            .run_scheduled_gc_cycle(finalized_height, eligibility_now, retry_schedule, || {
                retry_now
            })
            .map_err(|failure| failure.error)?
        {
            RetainedGcScheduledCycle::DeferredGlobal(_) => Ok(RetainedGcCycleTestReport {
                global_deferred: true,
                pending: 0,
                deferred: 0,
                completed: 0,
                pages: 0,
                item_failures: 0,
                retry_entries: retry_schedule.deadlines.len(),
            }),
            RetainedGcScheduledCycle::Ran(report) => Ok(RetainedGcCycleTestReport {
                global_deferred: false,
                pending: report.pending,
                deferred: report.deferred,
                completed: report.completed,
                pages: report.pages,
                item_failures: report.failures.len(),
                retry_entries: retry_schedule.deadlines.len(),
            }),
        }
    }
}
