//! Journal recovery, scheduled cycle outcomes and worker waiting.
use super::*;

#[derive(Default)]
struct JournalRecovery {
    failures: u32,
    next_attempt: Option<Instant>,
}

impl JournalRecovery {
    fn reset(&mut self) {
        self.failures = 0;
        self.next_attempt = None;
    }

    fn delay(&mut self, coordinator: &OcompRetentionCoordinator) -> Option<Duration> {
        match coordinator.status() {
            RetentionStatus::Unavailable { .. } => self.recover(coordinator),
            RetentionStatus::Quarantined { .. } => {
                self.reset();
                Some(RETAINED_GC_IDLE_POLL)
            }
            RetentionStatus::Empty | RetentionStatus::Ready(_) => {
                self.reset();
                None
            }
        }
    }

    fn recover(&mut self, coordinator: &OcompRetentionCoordinator) -> Option<Duration> {
        if let Some(next_attempt) = self.next_attempt {
            let now = Instant::now();
            if now < next_attempt {
                let remaining = next_attempt.saturating_duration_since(now);
                metrics::gauge!("outbe_ocomp_retention_journal_recovery_next_delay_seconds")
                    .set(remaining.as_secs_f64());
                return Some(remaining);
            }
        }
        match coordinator.recover_journal() {
            Ok(true) => {
                record_recovery_success();
                self.reset();
                None
            }
            Ok(false) => {
                self.reset();
                None
            }
            Err(error) => Some(self.failed(coordinator, &error)),
        }
    }

    fn failed(
        &mut self,
        coordinator: &OcompRetentionCoordinator,
        error: &RetentionError,
    ) -> Duration {
        record_journal_failure(error);
        let integrity_failure = matches!(coordinator.status(), RetentionStatus::Quarantined { .. });
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
            self.reset();
            return RETAINED_GC_IDLE_POLL;
        }
        self.failures = self.failures.saturating_add(1);
        metrics::gauge!("outbe_ocomp_retention_journal_recovery_consecutive_failures")
            .set(self.failures as f64);
        let delay = journal_recovery_backoff(self.failures.saturating_sub(1));
        metrics::gauge!("outbe_ocomp_retention_journal_recovery_next_delay_seconds")
            .set(delay.as_secs_f64());
        self.next_attempt = Some(Instant::now() + delay);
        tracing::warn!(
            %error,
            retry_delay_seconds = delay.as_secs(),
            journal_recovery_failures = self.failures,
            "OCOMP retention journal recovery failed; retrying with backoff"
        );

        delay
    }
}

fn record_recovery_success() {
    metrics::counter!(
        "outbe_ocomp_retention_journal_recovery_attempts_total",
        "result" => "success"
    )
    .increment(1);
    metrics::gauge!("outbe_ocomp_retention_journal_recovery_consecutive_failures").set(0.0);
    metrics::gauge!("outbe_ocomp_retention_journal_recovery_next_delay_seconds").set(0.0);
}

// A global failure retains the original worker's coordinator ownership while waiting.
enum CycleWait {
    Release(Duration),
    Retain(Duration),
}

fn run_cycle(
    coordinator: &OcompRetentionCoordinator,
    finalized_height: u64,
    retry_schedule: &mut RetainedGcRetrySchedule,
) -> CycleWait {
    let clock = RetainedGcClock {
        eligibility_now: Instant::now(),
        current_time: Instant::now,
    };
    match coordinator.run_scheduled_gc_cycle(finalized_height, retry_schedule, clock) {
        Ok(RetainedGcScheduledCycle::DeferredGlobal(delay)) => {
            metrics::gauge!("outbe_ocomp_retained_gc_global_retry_next_delay_seconds")
                .set(delay.as_secs_f64());
            CycleWait::Release(delay)
        }
        Ok(RetainedGcScheduledCycle::Ran(report)) => {
            metrics::gauge!("outbe_ocomp_retained_gc_global_retry_next_delay_seconds").set(0.0);
            metrics::counter!("outbe_ocomp_retained_gc_worker_cycles_total", "result" => "success")
                .increment(1);
            record_retained_gc_report(&report);
            let made_progress = report.completed != 0 || report.pages != 0;
            CycleWait::Release(crate::ocomp::retention::retained_gc_next_wake_delay(
                made_progress,
                report.next_retry_delay,
            ))
        }
        Err(failure) => {
            record_global_failure(&failure);
            CycleWait::Retain(RETAINED_GC_RETRY_BACKOFF)
        }
    }
}

fn record_global_failure(failure: &RetainedGcCycleFailure) {
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
}

pub(super) fn run_retained_gc_worker(
    coordinator: Weak<OcompRetentionCoordinator>,
    signal: Arc<RetainedGcSignal>,
) {
    let mut observed_epoch = 0_u64;
    let mut recovery = JournalRecovery::default();
    let mut retry_schedule = RetainedGcRetrySchedule::default();
    loop {
        let Some(coordinator) = coordinator.upgrade() else {
            return;
        };
        let finalized_height = signal.finalized_height.load(Ordering::Acquire);
        let closure_checkpoint = signal.closure_checkpoint.load(Ordering::Acquire);
        atomic_max(&coordinator.closure_checkpoint, closure_checkpoint);
        if let Some(delay) = recovery.delay(&coordinator) {
            drop(coordinator);
            wait_for_gc_signal(&signal, &mut observed_epoch, delay);
            continue;
        }
        match run_cycle(&coordinator, finalized_height, &mut retry_schedule) {
            CycleWait::Release(delay) => {
                drop(coordinator);
                wait_for_gc_signal(&signal, &mut observed_epoch, delay);
            }
            CycleWait::Retain(delay) => wait_for_gc_signal(&signal, &mut observed_epoch, delay),
        }
    }
}
