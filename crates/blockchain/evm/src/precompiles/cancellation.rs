//! Restores typed cancellation across the provider's string-only fatal channel.
#[cfg(test)]
use outbe_offchain_data::runtime_body_readers;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::{
    error::PrecompileError, projection::ExecutionReadCancelled, storage::SubCallError,
};
use parking_lot::Mutex;
use revm::context_interface::result::{AnyError, EVMError};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct ExecutionAbortBridge {
    terminal: Arc<Mutex<Option<ExecutionAbort>>>,
}

enum ExecutionAbort {
    ReadCancelled(ExecutionReadCancelled),
    CryptoBackendUnavailable(String),
}

impl ExecutionAbortBridge {
    pub(crate) fn clear(&self) {
        *self.terminal.lock() = None;
    }

    pub(crate) fn observe(&self, error: &PrecompileError, readers: Option<&RuntimeBodyReaders>) {
        // A handler cannot convert a failed native operation into a deterministic
        // revert or success by catching a child error. Clear only at call end.
        if matches!(
            *self.terminal.lock(),
            Some(ExecutionAbort::CryptoBackendUnavailable(_))
        ) {
            return;
        }
        match error {
            PrecompileError::BodyReadRequestDeadline => {
                *self.terminal.lock() = readers
                    .and_then(RuntimeBodyReaders::cancelled_read_budget)
                    .map(|budget| ExecutionAbort::ReadCancelled(ExecutionReadCancelled { budget }));
            }
            PrecompileError::CryptoBackendUnavailable(message) => {
                *self.terminal.lock() =
                    Some(ExecutionAbort::CryptoBackendUnavailable(message.clone()));
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
                "execution already stopped on a node-local infrastructure failure".into(),
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
        let abort = self.bridge.terminal.lock().take();
        match (result, abort) {
            (Err(EVMError::Custom(_)), Some(abort)) | (Ok(_), Some(abort)) => {
                let error = match abort {
                    ExecutionAbort::ReadCancelled(cancelled) => AnyError::new(cancelled),
                    ExecutionAbort::CryptoBackendUnavailable(message) => {
                        AnyError::new(PrecompileError::CryptoBackendUnavailable(message))
                    }
                };
                Err(EVMError::CustomAny(error))
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
    use outbe_offchain_storage::MemoryStorage;
    use outbe_primitives::projection::ExecutionReadBudget;

    #[test]
    fn crypto_abort_survives_caught_child_errors_and_does_not_leak() {
        let bridge = ExecutionAbortBridge::default();
        let call = bridge.begin_call();
        bridge.observe(
            &PrecompileError::CryptoBackendUnavailable("injected".into()),
            None,
        );
        bridge.observe(&PrecompileError::Revert("caught child error".into()), None);
        let result: Result<(), EVMError<std::convert::Infallible>> = Ok(());
        let error = call.finish(result);
        assert!(matches!(error, Err(EVMError::CustomAny(_))));
        assert!(bridge.check_subcall().is_ok());
        let result: Result<(), EVMError<std::convert::Infallible>> = Ok(());
        assert!(bridge.begin_call().finish(result).is_ok());
    }

    #[test]
    fn abort_marker_cannot_reclassify_a_database_or_later_provider_failure() {
        let readers = runtime_body_readers(Arc::new(MemoryStorage::new()));
        let budget = ExecutionReadBudget::new();
        let _guard = readers.enter_execution_budget(budget.clone());
        budget.cancel();
        let bridge = ExecutionAbortBridge::default();
        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::BodyReadRequestDeadline, Some(&readers));
        let database_error: Result<(), EVMError<std::io::Error>> = Err(EVMError::Database(
            std::io::Error::other("database failure"),
        ));
        assert!(matches!(
            call.finish(database_error),
            Err(EVMError::Database(_))
        ));

        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::BodyReadRequestDeadline, Some(&readers));
        bridge.observe(
            &PrecompileError::BodyReadCorruption("corrupt body".into()),
            Some(&readers),
        );
        let provider_error: Result<(), EVMError<std::convert::Infallible>> =
            Err(EVMError::Custom("provider failure".into()));
        assert!(matches!(
            call.finish(provider_error),
            Err(EVMError::Custom(_))
        ));
    }

    #[test]
    fn dropped_call_clears_its_abort_before_the_next_call() {
        let readers = runtime_body_readers(Arc::new(MemoryStorage::new()));
        let budget = ExecutionReadBudget::new();
        let _guard = readers.enter_execution_budget(budget.clone());
        budget.cancel();
        let bridge = ExecutionAbortBridge::default();
        let call = bridge.begin_call();
        bridge.observe(&PrecompileError::BodyReadRequestDeadline, Some(&readers));
        assert!(bridge.check_subcall().is_err());
        drop(call);
        assert!(bridge.check_subcall().is_ok());
        let error: Result<(), EVMError<std::convert::Infallible>> = Err(EVMError::Custom(
            "body read request deadline exceeded".into(),
        ));
        assert!(matches!(
            bridge.begin_call().finish(error),
            Err(EVMError::Custom(_))
        ));
    }
}
