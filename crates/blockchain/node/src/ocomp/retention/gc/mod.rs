use crate::ocomp::retention::*;

pub(super) mod retry;
mod worker;

use worker::run_retained_gc_worker;

const RETAINED_GC_PROGRESS_POLL: Duration = Duration::from_millis(100);

const RETAINED_GC_IDLE_POLL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(in crate::ocomp::retention) struct RetainedGcSignal {
    finalized_height: AtomicU64,
    closure_checkpoint: AtomicU64,
    epoch: Mutex<u64>,
    changed: Condvar,
}

impl RetainedGcSignal {
    pub(in crate::ocomp::retention) fn publish_finalized(&self, height: u64) {
        atomic_max(&self.finalized_height, height);
        self.wake();
    }

    pub(in crate::ocomp::retention) fn publish_closure(&self, height: u64) {
        atomic_max(&self.closure_checkpoint, height);
        self.wake();
    }

    pub(in crate::ocomp::retention) fn wake(&self) {
        let mut epoch = self.epoch.lock().unwrap_or_else(|error| error.into_inner());
        *epoch = epoch.wrapping_add(1);
        self.changed.notify_one();
    }
}

pub(in crate::ocomp::retention) fn atomic_max(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Acquire);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

pub(in crate::ocomp::retention) fn record_journal_failure(error: &RetentionError) {
    let (class, operation) = match error {
        RetentionError::Io { operation, .. } => ("io", *operation),
        RetentionError::AmbiguousJournal(_)
        | RetentionError::MalformedJournal(_)
        | RetentionError::UnsupportedJournalVersion { .. } => ("integrity", "validate"),
        _ => ("internal", "recover"),
    };
    metrics::counter!(
        "outbe_ocomp_retention_journal_failures_total",
        "class" => class,
        "operation" => operation
    )
    .increment(1);
}

pub(in crate::ocomp::retention) fn spawn_retained_gc_worker(
    coordinator: Weak<OcompRetentionCoordinator>,
    signal: Arc<RetainedGcSignal>,
) -> Result<(), RetentionError> {
    std::thread::Builder::new()
        .name("ocomp-retained-gc".to_owned())
        .spawn(move || run_retained_gc_worker(coordinator, signal))
        .map(|_| ())
        .map_err(RetentionError::RetainedTributeGcWorkerSpawn)
}

pub(crate) fn retained_gc_next_wake_delay(made_progress: bool) -> Duration {
    if made_progress {
        RETAINED_GC_PROGRESS_POLL
    } else {
        RETAINED_GC_IDLE_POLL
    }
}

fn record_retained_gc_report(report: &RetainedGcCycleReport) {
    metrics::gauge!("outbe_ocomp_retained_gc_pending_jobs").set(report.pending as f64);
    metrics::counter!("outbe_ocomp_retained_gc_completed_total").increment(report.completed);
}

fn wait_for_gc_signal(signal: &RetainedGcSignal, observed_epoch: &mut u64, delay: Duration) {
    let epoch = signal
        .epoch
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if *epoch != *observed_epoch {
        *observed_epoch = *epoch;
        return;
    }
    let (epoch, _) = signal
        .changed
        .wait_timeout(epoch, delay)
        .unwrap_or_else(|error| error.into_inner());
    *observed_epoch = *epoch;
}

pub(in crate::ocomp::retention) struct RetainedGcClock<F> {
    pub(in crate::ocomp::retention) eligibility_now: Instant,
    pub(in crate::ocomp::retention) current_time: F,
}

impl OcompRetentionCoordinator {
    fn run_gc_cycle(
        &self,
        finalized_height: u64,
    ) -> Result<RetainedGcCycleReport, RetainedGcCycleFailure> {
        let work_items =
            self.gc_candidate_work(finalized_height)
                .map_err(|error| RetainedGcCycleFailure {
                    class: RetainedGcFailureClass::Internal,
                    error,
                    report: None,
                })?;
        let mut report = RetainedGcCycleReport {
            pending: work_items.len(),
            completed: 0,
        };
        for work in work_items {
            match self.release_due_work(work, finalized_height) {
                Ok(RetainedGcAttemptOutcome::Completed(_)) => {
                    report.completed = report.completed.saturating_add(1);
                }
                Ok(RetainedGcAttemptOutcome::NoLongerPending) => {}
                Err(RetainedGcAttemptFailure::Global { class, error }) => {
                    return Err(RetainedGcCycleFailure {
                        class,
                        error,
                        report: Some(Box::new(report)),
                    });
                }
            }
        }
        Ok(report)
    }

    pub(in crate::ocomp::retention) fn run_scheduled_gc_cycle(
        &self,
        finalized_height: u64,
        retry_schedule: &mut RetainedGcRetrySchedule,
        mut clock: RetainedGcClock<impl FnMut() -> Instant>,
    ) -> Result<RetainedGcScheduledCycle, RetainedGcCycleFailure> {
        if let Some(delay) = retry_schedule.global_delay(clock.eligibility_now) {
            return Ok(RetainedGcScheduledCycle::DeferredGlobal(delay));
        }
        match self.run_gc_cycle(finalized_height) {
            Ok(report) => {
                retry_schedule.clear_global();
                Ok(RetainedGcScheduledCycle::Ran(report))
            }
            Err(failure) => {
                retry_schedule.defer_global((clock.current_time)());
                Err(failure)
            }
        }
    }
}
