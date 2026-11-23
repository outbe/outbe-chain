//! Preserves node-local execution failures across the provider fatal channel.
use evm2::{registry::HandlerError, AnyError};
use outbe_primitives::{
    error::PrecompileError, projection::NodeLocalExecutionAbort, storage::SubCallError,
};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct ExecutionAbortBridge {
    terminal: Arc<Mutex<Option<NodeLocalExecutionAbort>>>,
}

impl ExecutionAbortBridge {
    pub(crate) fn clear(&self) {
        *self.terminal.lock() = None;
    }

    pub(crate) fn observe(&self, error: &PrecompileError) {
        match error {
            PrecompileError::EnclaveUnavailable(reason)
            | PrecompileError::Storage(reason)
            | PrecompileError::SubCall(SubCallError::DatabaseError(reason)) => {
                *self.terminal.lock() = Some(NodeLocalExecutionAbort::new(reason.clone()));
            }
            // A child fatal is propagated by this provider into its caller.
            PrecompileError::SubCall(SubCallError::Fatal(_)) => {}
            _ => self.clear(),
        }
    }

    pub(crate) fn begin_call(&self) -> ExecutionCall {
        self.clear();
        ExecutionCall {
            bridge: self.clone(),
        }
    }

    pub(crate) fn check_subcall(&self) -> Result<(), SubCallError> {
        if self.terminal.lock().is_some() {
            return Err(SubCallError::Fatal(
                "execution already stopped on a node-local failure".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct ExecutionCall {
    bridge: ExecutionAbortBridge,
}

/// Transparent wrapper that exposes `E` via [`std::error::Error::source`] when stored inside
/// [`evm2::AnyError`] (whose `source()` delegates to the inner error's `source()`, skipping the
/// outer concrete type).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct SourceVisibleAbort<E: std::error::Error + Send + Sync + 'static>(
    #[source] pub(crate) E,
);

impl ExecutionCall {
    pub(crate) fn finish<T>(self, result: Result<T, HandlerError>) -> Result<T, HandlerError> {
        let terminal = self.bridge.terminal.lock().take();
        match (result, terminal) {
            (Err(HandlerError::Fatal(_)), Some(abort)) | (Ok(_), Some(abort)) => Err(
                HandlerError::Fatal(AnyError::new(SourceVisibleAbort(abort))),
            ),
            (result, _) => result,
        }
    }
}

/// Unwraps only a proven cancellation or node-local abort from an upstream
/// `InternalBlockExecutionError::EVM` so the engine retains its typed cause via
/// [`std::error::Error::source`].
pub(crate) fn preserve_cancelled_execution(
    error: reth_evm::BlockExecutionError,
) -> reth_evm::BlockExecutionError {
    use outbe_primitives::projection::ExecutionReadCancelled;
    use reth_evm::{BlockExecutionError, InternalBlockExecutionError};

    match error {
        BlockExecutionError::Internal(InternalBlockExecutionError::EVM { hash, error }) => {
            if ExecutionReadCancelled::find(&error).is_some()
                || NodeLocalExecutionAbort::find(&error).is_some()
            {
                return BlockExecutionError::Internal(InternalBlockExecutionError::Other(error));
            }
            if let Some(wrapped) = rewrap_direct_abort(&error) {
                return BlockExecutionError::Internal(InternalBlockExecutionError::Other(wrapped));
            }
            BlockExecutionError::Internal(InternalBlockExecutionError::EVM { hash, error })
        }
        error => error,
    }
}

fn rewrap_direct_abort(error: &AnyError) -> Option<AnyError> {
    use outbe_primitives::projection::ExecutionReadCancelled;

    let candidates = [
        Some(error),
        match error.downcast_ref::<HandlerError>() {
            Some(HandlerError::Fatal(inner)) => Some(inner),
            _ => None,
        },
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(cancelled) = candidate.downcast_ref::<ExecutionReadCancelled>() {
            return Some(AnyError::new(SourceVisibleAbort(cancelled.clone())));
        }
        if let Some(abort) = candidate.downcast_ref::<NodeLocalExecutionAbort>() {
            return Some(AnyError::new(SourceVisibleAbort(abort.clone())));
        }
    }
    None
}

impl Drop for ExecutionCall {
    fn drop(&mut self) {
        self.bridge.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evm2::DatabaseError;

    #[test]
    fn abort_marker_cannot_reclassify_a_database_or_later_provider_failure() {
        let bridge = ExecutionAbortBridge::default();
        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::EnclaveUnavailable(
            "enclave offline".into(),
        ));
        let database_error: Result<(), HandlerError> = Err(HandlerError::Database(
            DatabaseError::new(std::io::Error::other("database failure"), true),
        ));
        assert!(matches!(
            call.finish(database_error),
            Err(HandlerError::Database(_))
        ));

        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::EnclaveUnavailable(
            "enclave offline".into(),
        ));
        bridge.observe(&PrecompileError::Fatal("invalid result".into()));
        let provider_error: Result<(), HandlerError> =
            Err(HandlerError::Fatal(AnyError::from("provider failure")));
        let err = call
            .finish(provider_error)
            .expect_err("later provider failure must survive");
        assert!(matches!(err, HandlerError::Fatal(_)));
        assert!(NodeLocalExecutionAbort::find(&err).is_none());
    }

    #[test]
    fn dropped_call_clears_its_abort_before_the_next_call() {
        let bridge = ExecutionAbortBridge::default();
        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::EnclaveUnavailable(
            "enclave offline".into(),
        ));
        assert!(bridge.check_subcall().is_err());
        drop(call);
        assert!(bridge.check_subcall().is_ok());
        let error: Result<(), HandlerError> = Err(HandlerError::Fatal(AnyError::from(
            "provider fatal failure",
        )));
        let err = bridge
            .begin_call()
            .finish(error)
            .expect_err("provider fatal failure must survive");
        assert!(matches!(err, HandlerError::Fatal(_)));
        assert!(NodeLocalExecutionAbort::find(&err).is_none());
    }
}
