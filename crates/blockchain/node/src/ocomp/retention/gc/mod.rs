use crate::ocomp::retention::*;

pub(super) mod retry;

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

pub(crate) fn retained_gc_next_wake_delay(
    made_page_progress: bool,
    next_retry_delay: Option<Duration>,
) -> Duration {
    let progress_delay = made_page_progress.then_some(RETAINED_GC_PROGRESS_POLL);
    progress_delay
        .into_iter()
        .chain(next_retry_delay)
        .min()
        .unwrap_or(RETAINED_GC_IDLE_POLL)
        .min(RETAINED_GC_IDLE_POLL)
}

fn run_retained_gc_worker(
    coordinator: Weak<OcompRetentionCoordinator>,
    signal: Arc<RetainedGcSignal>,
) {
    let mut observed_epoch = 0_u64;
    let mut journal_recovery_failures = 0_u32;
    let mut next_journal_recovery: Option<Instant> = None;
    let mut retry_schedule = RetainedGcRetrySchedule::default();
    loop {
        let Some(coordinator) = coordinator.upgrade() else {
            return;
        };
        let finalized_height = signal.finalized_height.load(Ordering::Acquire);
        let closure_checkpoint = signal.closure_checkpoint.load(Ordering::Acquire);
        atomic_max(&coordinator.closure_checkpoint, closure_checkpoint);

        match coordinator.status() {
            RetentionStatus::Unavailable { .. } => {
                if let Some(next_attempt) = next_journal_recovery {
                    let now = Instant::now();
                    if now < next_attempt {
                        let remaining = next_attempt.saturating_duration_since(now);
                        metrics::gauge!(
                            "outbe_ocomp_retention_journal_recovery_next_delay_seconds"
                        )
                        .set(remaining.as_secs_f64());
                        drop(coordinator);
                        wait_for_gc_signal(&signal, &mut observed_epoch, remaining);
                        continue;
                    }
                }
                match coordinator.recover_journal() {
                    Ok(true) => {
                        metrics::counter!(
                            "outbe_ocomp_retention_journal_recovery_attempts_total",
                            "result" => "success"
                        )
                        .increment(1);
                        metrics::gauge!(
                            "outbe_ocomp_retention_journal_recovery_consecutive_failures"
                        )
                        .set(0.0);
                        metrics::gauge!(
                            "outbe_ocomp_retention_journal_recovery_next_delay_seconds"
                        )
                        .set(0.0);
                        journal_recovery_failures = 0;
                        next_journal_recovery = None;
                    }
                    Ok(false) => {
                        journal_recovery_failures = 0;
                        next_journal_recovery = None;
                    }
                    Err(error) => {
                        record_journal_failure(&error);
                        let integrity_failure =
                            matches!(coordinator.status(), RetentionStatus::Quarantined { .. });
                        let result = if integrity_failure {
                            "integrity_error"
                        } else {
                            "io_error"
                        };
                        metrics::counter!(
                            "outbe_ocomp_retention_journal_recovery_attempts_total",
                            "result" => result
                        )
                        .increment(1);
                        if integrity_failure {
                            journal_recovery_failures = 0;
                            next_journal_recovery = None;
                            drop(coordinator);
                            wait_for_gc_signal(&signal, &mut observed_epoch, RETAINED_GC_IDLE_POLL);
                            continue;
                        }
                        journal_recovery_failures = journal_recovery_failures.saturating_add(1);
                        metrics::gauge!(
                            "outbe_ocomp_retention_journal_recovery_consecutive_failures"
                        )
                        .set(journal_recovery_failures as f64);
                        let delay =
                            journal_recovery_backoff(journal_recovery_failures.saturating_sub(1));
                        metrics::gauge!(
                            "outbe_ocomp_retention_journal_recovery_next_delay_seconds"
                        )
                        .set(delay.as_secs_f64());
                        next_journal_recovery = Some(Instant::now() + delay);
                        tracing::warn!(
                            %error,
                            retry_delay_seconds = delay.as_secs(),
                            journal_recovery_failures,
                            "OCOMP retention journal recovery failed; retrying with backoff"
                        );
                        drop(coordinator);
                        wait_for_gc_signal(&signal, &mut observed_epoch, delay);
                        continue;
                    }
                }
            }
            RetentionStatus::Quarantined { .. } => {
                journal_recovery_failures = 0;
                next_journal_recovery = None;
                drop(coordinator);
                wait_for_gc_signal(&signal, &mut observed_epoch, RETAINED_GC_IDLE_POLL);
                continue;
            }
            RetentionStatus::Empty | RetentionStatus::Ready(_) => {
                journal_recovery_failures = 0;
                next_journal_recovery = None;
            }
        }

        let cycle_started_at = Instant::now();
        let report = match coordinator.run_scheduled_gc_cycle(
            finalized_height,
            cycle_started_at,
            &mut retry_schedule,
            Instant::now,
        ) {
            Ok(RetainedGcScheduledCycle::DeferredGlobal(delay)) => {
                metrics::gauge!("outbe_ocomp_retained_gc_global_retry_next_delay_seconds")
                    .set(delay.as_secs_f64());
                drop(coordinator);
                wait_for_gc_signal(&signal, &mut observed_epoch, delay);
                continue;
            }
            Ok(RetainedGcScheduledCycle::Ran(report)) => {
                metrics::gauge!("outbe_ocomp_retained_gc_global_retry_next_delay_seconds").set(0.0);
                report
            }
            Err(failure) => {
                if let Some(report) = &failure.report {
                    record_retained_gc_report(report);
                }
                metrics::counter!("outbe_ocomp_retained_gc_errors_total").increment(1);
                metrics::gauge!("outbe_ocomp_retained_gc_global_retry_next_delay_seconds")
                    .set(RETAINED_GC_RETRY_BACKOFF.as_secs_f64());
                metrics::counter!(
                    "outbe_ocomp_retained_gc_retry_attempts_total",
                    "scope" => "global",
                    "failure_class" => failure.class.as_str()
                )
                .increment(1);
                metrics::counter!(
                    "outbe_ocomp_retained_gc_failures_total",
                    "scope" => "global",
                    "failure_class" => failure.class.as_str()
                )
                .increment(1);
                metrics::counter!(
                    "outbe_ocomp_retained_gc_worker_cycles_total",
                    "result" => "global_error",
                    "failure_class" => failure.class.as_str()
                )
                .increment(1);
                tracing::warn!(
                    failure_class = failure.class.as_str(),
                    error = %failure.error,
                    "OCOMP retained-input GC global failure; retrying independently of ExEx"
                );
                wait_for_gc_signal(&signal, &mut observed_epoch, RETAINED_GC_RETRY_BACKOFF);
                continue;
            }
        };
        metrics::counter!(
            "outbe_ocomp_retained_gc_worker_cycles_total",
            "result" => "success"
        )
        .increment(1);
        record_retained_gc_report(&report);
        let made_progress = report.completed != 0 || report.pages != 0;
        let delay = crate::ocomp::retention::retained_gc_next_wake_delay(
            made_progress,
            report.next_retry_delay,
        );
        drop(coordinator);
        wait_for_gc_signal(&signal, &mut observed_epoch, delay);
    }
}

fn record_retained_gc_report(report: &RetainedGcCycleReport) {
    metrics::gauge!("outbe_ocomp_retained_gc_pending_jobs").set(report.pending as f64);
    metrics::gauge!("outbe_ocomp_retained_gc_deferred_jobs").set(report.deferred as f64);
    metrics::gauge!("outbe_ocomp_retained_gc_retry_next_delay_seconds").set(
        report
            .next_retry_delay
            .map_or(0.0, |delay| delay.as_secs_f64()),
    );
    metrics::counter!("outbe_ocomp_retained_gc_page_attempts_total")
        .increment(report.completed + report.pages + report.failures.len() as u64);
    metrics::counter!("outbe_ocomp_retained_gc_completed_total").increment(report.completed);
    metrics::counter!("outbe_ocomp_retained_gc_pages_total").increment(report.pages);
    for failure in &report.failures {
        metrics::counter!("outbe_ocomp_retained_gc_errors_total").increment(1);
        metrics::counter!(
            "outbe_ocomp_retained_gc_retry_attempts_total",
            "scope" => "item",
            "failure_class" => failure.class.as_str()
        )
        .increment(1);
        metrics::counter!(
            "outbe_ocomp_retained_gc_failures_total",
            "scope" => "item",
            "failure_class" => failure.class.as_str()
        )
        .increment(1);
        tracing::warn!(
            candidate_block_hash = %failure.work.key,
            generation = failure.work.generation,
            failure_class = failure.class.as_str(),
            error = %failure.error,
            retry_delay_seconds = RETAINED_GC_RETRY_BACKOFF.as_secs(),
            "OCOMP retained-input GC failed for one lease; other leases continue"
        );
    }
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

impl OcompRetentionCoordinator {
    fn run_gc_cycle(
        &self,
        finalized_height: u64,
        now: Instant,
        retry_schedule: &mut RetainedGcRetrySchedule,
        current_time: &mut impl FnMut() -> Instant,
    ) -> Result<RetainedGcCycleReport, RetainedGcCycleFailure> {
        let work_items =
            self.gc_candidate_work(finalized_height)
                .map_err(|error| RetainedGcCycleFailure {
                    class: RetainedGcFailureClass::Internal,
                    error,
                    report: None,
                })?;
        retry_schedule.retain_pending(&work_items);
        let mut report = RetainedGcCycleReport {
            pending: work_items.len(),
            deferred: 0,
            completed: 0,
            pages: 0,
            failures: Vec::new(),
            next_retry_delay: None,
        };
        for work in work_items {
            if !retry_schedule.is_eligible(work, now) {
                report.deferred = report.deferred.saturating_add(1);
                continue;
            }
            match self.release_due_work(work, finalized_height) {
                Ok(RetainedGcAttemptOutcome::Completed(_)) => {
                    retry_schedule.clear(work);
                    report.completed = report.completed.saturating_add(1);
                }
                Ok(RetainedGcAttemptOutcome::PageProgress) => {
                    retry_schedule.clear(work);
                    report.pages = report.pages.saturating_add(1);
                }
                Ok(RetainedGcAttemptOutcome::NoLongerPending) => {
                    retry_schedule.clear(work);
                }
                Err(RetainedGcAttemptFailure::Item(failure)) => {
                    retry_schedule.defer(failure.work, current_time());
                    report.deferred = report.deferred.saturating_add(1);
                    report.failures.push(failure);
                }
                Err(RetainedGcAttemptFailure::Global { class, error }) => {
                    report.next_retry_delay = retry_schedule.next_delay(current_time());
                    return Err(RetainedGcCycleFailure {
                        class,
                        error,
                        report: Some(Box::new(report)),
                    });
                }
            }
        }
        report.next_retry_delay = retry_schedule.next_delay(current_time());
        Ok(report)
    }

    pub(in crate::ocomp::retention) fn run_scheduled_gc_cycle(
        &self,
        finalized_height: u64,
        now: Instant,
        retry_schedule: &mut RetainedGcRetrySchedule,
        mut current_time: impl FnMut() -> Instant,
    ) -> Result<RetainedGcScheduledCycle, RetainedGcCycleFailure> {
        if let Some(delay) = retry_schedule.global_delay(now) {
            return Ok(RetainedGcScheduledCycle::DeferredGlobal(delay));
        }
        match self.run_gc_cycle(finalized_height, now, retry_schedule, &mut current_time) {
            Ok(report) => {
                retry_schedule.clear_global();
                Ok(RetainedGcScheduledCycle::Ran(report))
            }
            Err(failure) => {
                retry_schedule.defer_global(current_time());
                Err(failure)
            }
        }
    }
}
