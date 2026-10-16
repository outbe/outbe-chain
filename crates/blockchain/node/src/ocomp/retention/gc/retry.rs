use crate::ocomp::retention::*;

pub(in crate::ocomp::retention) const RETAINED_GC_RETRY_BACKOFF: Duration = Duration::from_secs(5);

const JOURNAL_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_secs(1);

const JOURNAL_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RetainedGcWorkId {
    pub(in crate::ocomp::retention) key: B256,
    pub(in crate::ocomp::retention) generation: u64,
}

#[derive(Default)]
pub(crate) struct RetainedGcRetrySchedule {
    global_deadline: Option<Instant>,
}

impl RetainedGcRetrySchedule {
    pub(in crate::ocomp::retention) fn defer_global(&mut self, now: Instant) {
        self.global_deadline = Some(now + RETAINED_GC_RETRY_BACKOFF);
    }

    pub(in crate::ocomp::retention) fn clear_global(&mut self) {
        self.global_deadline = None;
    }

    pub(in crate::ocomp::retention) fn global_delay(&self, now: Instant) -> Option<Duration> {
        self.global_deadline
            .filter(|deadline| *deadline > now)
            .map(|deadline| deadline.saturating_duration_since(now))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ocomp::retention) enum RetainedGcFailureClass {
    Internal,
}

impl RetainedGcFailureClass {
    pub(in crate::ocomp::retention) const fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
        }
    }
}

pub(in crate::ocomp::retention) struct RetainedGcCycleFailure {
    pub(in crate::ocomp::retention) class: RetainedGcFailureClass,
    pub(in crate::ocomp::retention) error: RetentionError,
    pub(in crate::ocomp::retention) report: Option<Box<RetainedGcCycleReport>>,
}

pub(in crate::ocomp::retention) enum RetainedGcAttemptFailure {
    Global {
        class: RetainedGcFailureClass,
        error: RetentionError,
    },
}

impl RetainedGcAttemptFailure {
    pub(in crate::ocomp::retention) fn global(error: RetentionError) -> Self {
        Self::Global {
            class: RetainedGcFailureClass::Internal,
            error,
        }
    }

    pub(in crate::ocomp::retention) fn into_error(self) -> RetentionError {
        match self {
            Self::Global { error, .. } => error,
        }
    }
}

pub(in crate::ocomp::retention) enum RetainedGcAttemptOutcome {
    Completed(DurablePinAck),
    NoLongerPending,
}

pub(in crate::ocomp::retention) struct RetainedGcCycleReport {
    pub(in crate::ocomp::retention) pending: usize,
    pub(in crate::ocomp::retention) completed: u64,
}

pub(in crate::ocomp::retention) enum RetainedGcScheduledCycle {
    DeferredGlobal(Duration),
    Ran(RetainedGcCycleReport),
}

pub(crate) fn journal_recovery_backoff(consecutive_failures: u32) -> Duration {
    let seconds = match consecutive_failures {
        0..=5 => 1_u64 << consecutive_failures,
        _ => JOURNAL_RECOVERY_MAX_BACKOFF.as_secs(),
    };
    Duration::from_secs(seconds).max(JOURNAL_RECOVERY_INITIAL_BACKOFF)
}
