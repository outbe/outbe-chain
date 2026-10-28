//! Preserves node-local execution failures across the provider fatal channel.
use outbe_primitives::{
    error::PrecompileError, projection::NodeLocalExecutionAbort, storage::SubCallError,
};
use parking_lot::Mutex;
use revm::context_interface::result::{AnyError, EVMError};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct ExecutionAbortBridge {
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

impl ExecutionCall {
    pub(crate) fn finish<T, D>(self, result: Result<T, EVMError<D>>) -> Result<T, EVMError<D>> {
        let terminal = self.bridge.terminal.lock().take();
        match (result, terminal) {
            (Err(EVMError::Custom(_)), Some(abort)) | (Ok(_), Some(abort)) => {
                Err(EVMError::CustomAny(AnyError::new(abort)))
            }
            (result, _) => result,
        }
    }
}

impl Drop for ExecutionCall {
    fn drop(&mut self) {
        self.bridge.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_marker_cannot_reclassify_a_database_or_later_provider_failure() {
        let bridge = ExecutionAbortBridge::default();
        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::EnclaveUnavailable(
            "enclave offline".into(),
        ));
        let database_error: Result<(), EVMError<std::io::Error>> = Err(EVMError::Database(
            std::io::Error::other("database failure"),
        ));
        assert!(matches!(
            call.finish(database_error),
            Err(EVMError::Database(_))
        ));

        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::EnclaveUnavailable(
            "enclave offline".into(),
        ));
        bridge.observe(&PrecompileError::Fatal("invalid result".into()));
        let provider_error: Result<(), EVMError<std::convert::Infallible>> =
            Err(EVMError::Custom("provider failure".into()));
        assert!(matches!(
            call.finish(provider_error),
            Err(EVMError::Custom(_))
        ));
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
        let error: Result<(), EVMError<std::convert::Infallible>> =
            Err(EVMError::Custom("provider fatal failure".into()));
        assert!(matches!(
            bridge.begin_call().finish(error),
            Err(EVMError::Custom(_))
        ));
    }
}
