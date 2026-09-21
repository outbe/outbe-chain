use super::super::publish_status;
#[cfg(test)]
use super::super::readiness_checkpoint;
use super::super::FinalizedTarget;
use super::super::ProjectionExit;
use super::HistoricalProjectionDataError;
#[cfg(test)]
use metrics::counter;
use outbe_offchain_data::ProjectionFailure;
use outbe_offchain_data::ProjectionFailureClass;
use outbe_offchain_data::ProjectionReadinessPublisher;
use outbe_offchain_data::ProjectionStatus;
use outbe_offchain_storage::StorageError;
use outbe_offchain_storage::StorageErrorKind;
use outbe_primitives::projection::ProjectionCheckpoint;
use std::sync::Arc;

#[cfg(test)]
pub(in super::super) fn publish_progress(
    publisher: &ProjectionReadinessPublisher,
    checkpoint: Option<ProjectionCheckpoint>,
    pending: Option<FinalizedTarget>,
) {
    let caught_up = match (checkpoint, pending) {
        (Some(checkpoint), Some(pending)) => {
            checkpoint.block_number == pending.number && checkpoint.block_hash == pending.hash
        }
        (Some(_), None) => true,
        (None, _) => false,
    };
    publish_status(
        publisher,
        match (caught_up, checkpoint) {
            (true, Some(checkpoint)) => ProjectionStatus::Ready { checkpoint },
            (_, checkpoint) => ProjectionStatus::CatchingUp { checkpoint },
        },
        pending,
    );
}

#[cfg(test)]
pub(in super::super) fn publish_fatal(
    publisher: &ProjectionReadinessPublisher,
    exit: &tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
    class: ProjectionFailureClass,
    message: impl Into<Arc<str>>,
) {
    let failure = ProjectionFailure::new(class, message);
    publish_projection_failure(publisher, exit, failure);
}

#[cfg(test)]
pub(in super::super) fn publish_projection_failure(
    publisher: &ProjectionReadinessPublisher,
    exit: &tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
    failure: ProjectionFailure,
) {
    let class = failure.class;
    let checkpoint = readiness_checkpoint(&publisher.current());
    publish_status(
        publisher,
        ProjectionStatus::Fatal {
            checkpoint,
            error: failure.clone(),
        },
        None,
    );
    counter!("outbe_projection_failures_total", "class" => format!("{class:?}")).increment(1);
    let _ = exit.send(ProjectionExit { failure });
}

#[cfg(test)]
pub(in super::super) fn projection_is_unavailable(error: &eyre::Report) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<StorageError>()
            .is_some_and(|storage| storage.kind() == StorageErrorKind::Unavailable)
    })
}

#[cfg(test)]
pub(in super::super) fn projection_failure_class(error: &eyre::Report) -> ProjectionFailureClass {
    if let Some(storage) = error
        .chain()
        .find_map(|source| source.downcast_ref::<StorageError>())
    {
        return match storage.kind() {
            StorageErrorKind::Corruption => ProjectionFailureClass::CorruptBody,
            StorageErrorKind::WriterLeaseLost => ProjectionFailureClass::WriterLeaseLost,
            StorageErrorKind::InvalidArgument => ProjectionFailureClass::StorageInvalidArgument,
            StorageErrorKind::Unavailable => ProjectionFailureClass::StorageUnavailable,
            StorageErrorKind::Backend => ProjectionFailureClass::StorageBackend,
            StorageErrorKind::RequestDeadline => ProjectionFailureClass::StorageRequestDeadline,
        };
    }
    if error.chain().any(|source| {
        source
            .downcast_ref::<outbe_offchain_data::ProjectionError>()
            .is_some()
    }) {
        ProjectionFailureClass::MalformedEvent
    } else if error.chain().any(|source| {
        source
            .downcast_ref::<HistoricalProjectionDataError>()
            .is_some()
    }) {
        ProjectionFailureClass::HistoricalReceiptsUnavailable
    } else {
        ProjectionFailureClass::Other
    }
}
