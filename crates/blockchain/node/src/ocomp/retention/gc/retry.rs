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
    pub(in crate::ocomp::retention) deadlines: HashMap<RetainedGcWorkId, Instant>,
    global_deadline: Option<Instant>,
}

impl RetainedGcRetrySchedule {
    pub(in crate::ocomp::retention) fn retain_pending(&mut self, pending: &[RetainedGcWorkId]) {
        let pending = pending.iter().copied().collect::<HashSet<_>>();
        self.deadlines.retain(|work, _| pending.contains(work));
    }

    pub(in crate::ocomp::retention) fn is_eligible(
        &self,
        work: RetainedGcWorkId,
        now: Instant,
    ) -> bool {
        self.deadlines
            .get(&work)
            .is_none_or(|deadline| *deadline <= now)
    }

    pub(in crate::ocomp::retention) fn defer(&mut self, work: RetainedGcWorkId, now: Instant) {
        self.deadlines.insert(work, now + RETAINED_GC_RETRY_BACKOFF);
    }

    pub(in crate::ocomp::retention) fn clear(&mut self, work: RetainedGcWorkId) {
        self.deadlines.remove(&work);
    }

    pub(in crate::ocomp::retention) fn next_delay(&self, now: Instant) -> Option<Duration> {
        self.deadlines
            .values()
            .map(|deadline| deadline.saturating_duration_since(now))
            .min()
    }

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
    ItemData,
    StorageUnavailable,
    StorageBackend,
    StorageDeadline,
    WriterLeaseLost,
    Internal,
}

impl RetainedGcFailureClass {
    pub(in crate::ocomp::retention) const fn as_str(self) -> &'static str {
        match self {
            Self::ItemData => "item_data",
            Self::StorageUnavailable => "storage_unavailable",
            Self::StorageBackend => "storage_backend",
            Self::StorageDeadline => "storage_deadline",
            Self::WriterLeaseLost => "writer_lease_lost",
            Self::Internal => "internal",
        }
    }
}

pub(in crate::ocomp::retention) struct RetainedGcItemFailure {
    pub(in crate::ocomp::retention) work: RetainedGcWorkId,
    pub(in crate::ocomp::retention) class: RetainedGcFailureClass,
    pub(in crate::ocomp::retention) error: RetentionError,
}

pub(in crate::ocomp::retention) struct RetainedGcCycleFailure {
    pub(in crate::ocomp::retention) class: RetainedGcFailureClass,
    pub(in crate::ocomp::retention) error: RetentionError,
    pub(in crate::ocomp::retention) report: Option<Box<RetainedGcCycleReport>>,
}

pub(in crate::ocomp::retention) enum RetainedGcAttemptFailure {
    Item(RetainedGcItemFailure),
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
            Self::Item(failure) => failure.error,
            Self::Global { error, .. } => error,
        }
    }
}

pub(in crate::ocomp::retention) enum RetainedGcAttemptOutcome {
    Completed(DurablePinAck),
    PageProgress,
    NoLongerPending,
}

pub(in crate::ocomp::retention) struct RetainedGcCycleReport {
    pub(in crate::ocomp::retention) pending: usize,
    pub(in crate::ocomp::retention) deferred: usize,
    pub(in crate::ocomp::retention) completed: u64,
    pub(in crate::ocomp::retention) pages: u64,
    pub(in crate::ocomp::retention) failures: Vec<RetainedGcItemFailure>,
    pub(in crate::ocomp::retention) next_retry_delay: Option<Duration>,
}

pub(in crate::ocomp::retention) enum RetainedGcScheduledCycle {
    DeferredGlobal(Duration),
    Ran(RetainedGcCycleReport),
}

pub(in crate::ocomp::retention) fn classify_retained_gc_failure(
    key: B256,
    generation: u64,
    source: TributeRepositoryError,
) -> RetainedGcAttemptFailure {
    let (item_local, class) = match &source {
        TributeRepositoryError::Storage(storage) => match storage.kind() {
            StorageErrorKind::Corruption => (true, RetainedGcFailureClass::ItemData),
            StorageErrorKind::Unavailable => (false, RetainedGcFailureClass::StorageUnavailable),
            StorageErrorKind::Backend => (false, RetainedGcFailureClass::StorageBackend),
            StorageErrorKind::RequestDeadline => (false, RetainedGcFailureClass::StorageDeadline),
            StorageErrorKind::WriterLeaseLost => (false, RetainedGcFailureClass::WriterLeaseLost),
            StorageErrorKind::InvalidArgument => (false, RetainedGcFailureClass::Internal),
        },
        TributeRepositoryError::CanonicalBody(_)
        | TributeRepositoryError::MalformedIndexKey { .. }
        | TributeRepositoryError::NonEmptyIndexValue { .. }
        | TributeRepositoryError::RetainedDayMismatch { .. }
        | TributeRepositoryError::RetainedIdentity(_)
        | TributeRepositoryError::RetainedCommitment(_)
        | TributeRepositoryError::ConflictingRetainedBody { .. }
        | TributeRepositoryError::RetainedCommitmentMismatch { .. }
        | TributeRepositoryError::RetainedMetadata { .. }
        | TributeRepositoryError::DanglingRetainedIndex { .. }
        | TributeRepositoryError::MissingRetainedIndex { .. }
        | TributeRepositoryError::NonAscendingRetainedPage { .. }
        | TributeRepositoryError::InvalidRetainedCursor { .. }
        | TributeRepositoryError::InvalidRetainedContinuation { .. }
        | TributeRepositoryError::RetainedNamespaceMismatch { .. } => {
            (true, RetainedGcFailureClass::ItemData)
        }
        _ => (false, RetainedGcFailureClass::Internal),
    };
    let error = RetentionError::RetainedTributeGc(source.to_string());
    if item_local {
        RetainedGcAttemptFailure::Item(RetainedGcItemFailure {
            work: RetainedGcWorkId { key, generation },
            class,
            error,
        })
    } else {
        RetainedGcAttemptFailure::Global { class, error }
    }
}

pub(crate) fn journal_recovery_backoff(consecutive_failures: u32) -> Duration {
    let seconds = match consecutive_failures {
        0..=5 => 1_u64 << consecutive_failures,
        _ => JOURNAL_RECOVERY_MAX_BACKOFF.as_secs(),
    };
    Duration::from_secs(seconds).max(JOURNAL_RECOVERY_INITIAL_BACKOFF)
}
