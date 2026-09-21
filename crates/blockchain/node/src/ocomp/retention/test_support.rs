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
