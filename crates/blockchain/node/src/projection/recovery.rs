use super::storage_failure_class;
use outbe_offchain_data::ProjectionFailure;
use outbe_offchain_data::RuntimeBodyFailure;
use outbe_offchain_storage::StorageErrorKind;
use outbe_offchain_storage::StorageWriterHandle;

#[derive(Clone)]
pub struct ProjectionRuntimeRecoveryHandle {
    pub(super) writer: StorageWriterHandle,
    pub(super) failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionRuntimeRecoveryV1 {
    Recovered,
    Unavailable,
    Fatal(ProjectionFailure),
}

impl ProjectionRuntimeRecoveryHandle {
    /// Proves that the shared offchain storage backend can start transactions again, then closes only the
    /// transient runtime-read outage. A fatal body failure is sticky and is never cleared here.
    pub fn reconcile(&self, generation: u64) -> ProjectionRuntimeRecoveryV1 {
        if let Err(error) = self.writer.verify_transaction_capability() {
            return if error.kind() == StorageErrorKind::Unavailable {
                ProjectionRuntimeRecoveryV1::Unavailable
            } else {
                ProjectionRuntimeRecoveryV1::Fatal(ProjectionFailure::new(
                    storage_failure_class(&error),
                    format!("offchain runtime-body storage recovery failed: {error}"),
                ))
            };
        }
        let cleared = self.failure_sender.send_if_modified(|current| {
            if matches!(
                current,
                Some(RuntimeBodyFailure::Unavailable {
                    generation: current_generation,
                    ..
                }) if *current_generation == generation
            ) {
                *current = None;
                true
            } else {
                false
            }
        });
        if cleared {
            return ProjectionRuntimeRecoveryV1::Recovered;
        }
        match self.failure_sender.borrow().clone() {
            Some(RuntimeBodyFailure::Unavailable { .. }) => {
                ProjectionRuntimeRecoveryV1::Unavailable
            }
            Some(RuntimeBodyFailure::Fatal(failure)) => ProjectionRuntimeRecoveryV1::Fatal(failure),
            None => ProjectionRuntimeRecoveryV1::Recovered,
        }
    }
}
