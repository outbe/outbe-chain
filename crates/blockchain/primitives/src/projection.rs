//! Backend-neutral local projection checkpoint and execution-read budget types.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

/// Local-only lifetime of one consensus execution request.
///
/// Commonware owns the actual proposal/verification deadline. The application
/// cancels this token when Commonware drops the corresponding response channel.
/// Synchronous storage reads thus inherit the already-running request lifetime.
/// They do not invent or reset a wall-clock deadline.
#[derive(Clone, Debug, Default)]
pub struct ExecutionReadBudget {
    cancelled: Arc<AtomicBool>,
}

impl ExecutionReadBudget {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Whether two handles belong to the same local execution request.
    pub fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled)
    }
}

/// Positive evidence that a particular execution stopped on a cancelled body read.
/// This is an internal abort, never a block-validity verdict.
#[derive(Clone, Debug, thiserror::Error)]
#[error("execution stopped on a cancelled body read")]
pub struct ExecutionReadCancelled {
    pub budget: ExecutionReadBudget,
}

impl ExecutionReadCancelled {
    /// Finds the typed abort through adapters without parsing error messages.
    pub fn find<'a>(mut error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        loop {
            if let Some(cancelled) = error.downcast_ref::<Self>() {
                return Some(cancelled);
            }
            error = error.source()?;
        }
    }
}

/// Positive evidence that execution stopped on a node-local dependency failure
/// (such as local TEE sidecar unavailability, off-chain body reader unavailability,
/// or native storage unavailability). This is a local abort, never a
/// consensus block-validity verdict or a reason to crash the node process.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("execution stopped on a node-local dependency failure: {reason}")]
pub struct NodeLocalExecutionAbort {
    pub reason: String,
}

impl NodeLocalExecutionAbort {
    /// Finds the typed node-local abort through adapters without parsing error messages.
    pub fn find<'a>(mut error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        loop {
            if let Some(aborted) = error.downcast_ref::<Self>() {
                return Some(aborted);
            }
            error = error.source()?;
        }
    }
}

/// Exact block identity through which one projection view has applied all events.
///
/// The owner defines durability: consensus readiness uses the logical overlay boundary, while
/// durable storage persists the same identity as its durable replay cursor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectionCheckpoint {
    pub block_number: u64,
    pub block_hash: B256,
}

/// Stable class for a deterministic projection failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionFailureClass {
    MalformedEvent,
    UnsupportedSchema,
    WrongNetwork,
    CheckpointMismatch,
    UnmanagedData,
    HistoricalReceiptsUnavailable,
    CorruptBody,
    DanglingIndex,
    ProjectorExited,
    ReadinessChannelClosed,
    StorageReconnectDeadline,
    WriterLeaseLost,
    StorageInvalidArgument,
    StorageUnavailable,
    StorageBackend,
    StorageRequestDeadline,
    Other,
}
