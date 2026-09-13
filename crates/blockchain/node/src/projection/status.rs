use super::FinalizedTarget;
use super::ProjectionWriteDeadlineError;
use metrics::gauge;
use outbe_offchain_data::ProjectionFailureClass;
use outbe_offchain_data::ProjectionReadinessPublisher;
use outbe_offchain_data::ProjectionStatus;
use outbe_offchain_storage::StorageError;
use outbe_offchain_storage::StorageErrorKind;
use outbe_primitives::projection::ProjectionCheckpoint;

#[must_use]
pub fn projection_frame_failure_class(error: &eyre::Report) -> ProjectionFailureClass {
    if error
        .chain()
        .any(|cause| cause.is::<ProjectionWriteDeadlineError>())
    {
        return ProjectionFailureClass::MongoReconnectDeadline;
    }
    if let Some(storage) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<StorageError>())
    {
        return storage_failure_class(storage);
    }
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<outbe_offchain_data::ProjectionError>()
            .is_some()
    }) {
        ProjectionFailureClass::MalformedEvent
    } else {
        ProjectionFailureClass::Other
    }
}

pub(super) fn storage_failure_class(error: &StorageError) -> ProjectionFailureClass {
    match error.kind() {
        StorageErrorKind::Corruption => ProjectionFailureClass::CorruptBody,
        StorageErrorKind::WriterLeaseLost => ProjectionFailureClass::WriterLeaseLost,
        StorageErrorKind::InvalidArgument => ProjectionFailureClass::StorageInvalidArgument,
        StorageErrorKind::Unavailable => ProjectionFailureClass::StorageUnavailable,
        StorageErrorKind::Backend => ProjectionFailureClass::StorageBackend,
        StorageErrorKind::RequestDeadline => ProjectionFailureClass::StorageRequestDeadline,
    }
}

pub(super) fn readiness_checkpoint(status: &ProjectionStatus) -> Option<ProjectionCheckpoint> {
    match status {
        ProjectionStatus::CatchingUp { checkpoint }
        | ProjectionStatus::MongoUnavailable { checkpoint, .. } => *checkpoint,
        ProjectionStatus::Ready { checkpoint } => Some(*checkpoint),
        ProjectionStatus::Fatal { checkpoint, .. } => *checkpoint,
        ProjectionStatus::Starting => None,
    }
}

pub(super) fn publish_status(
    publisher: &ProjectionReadinessPublisher,
    status: ProjectionStatus,
    target: Option<FinalizedTarget>,
) {
    let (status_code, ready) = match &status {
        ProjectionStatus::Starting => (0.0, 0.0),
        ProjectionStatus::CatchingUp { .. } => (1.0, 0.0),
        ProjectionStatus::MongoUnavailable { .. } => (2.0, 0.0),
        ProjectionStatus::Ready { .. } => (3.0, 1.0),
        ProjectionStatus::Fatal { .. } => (4.0, 0.0),
    };
    gauge!("outbe_projection_status").set(status_code);
    gauge!("outbe_projection_readiness").set(ready);
    gauge!("outbe_projection_validator_participation_gate").set(ready);
    if let Some(checkpoint) = readiness_checkpoint(&status) {
        gauge!("outbe_projection_checkpoint_number").set(checkpoint.block_number as f64);
        if let Some(target) = target {
            gauge!("outbe_projection_lag_blocks")
                .set(target.number.saturating_sub(checkpoint.block_number) as f64);
        }
    }
    if ready > 0.0 {
        gauge!("outbe_projection_storage_reconnect_active").set(0.0);
        gauge!("outbe_projection_storage_reconnect_remaining_seconds").set(0.0);
    }
    publisher.publish(status);
}
