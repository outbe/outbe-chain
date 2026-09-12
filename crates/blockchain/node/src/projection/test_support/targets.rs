use super::super::FinalizedTarget;
use super::super::ProjectionExit;
use super::publish_fatal;
use eyre::bail;
use outbe_offchain_data::ProjectionFailureClass;
use outbe_offchain_data::ProjectionReadinessPublisher;
#[cfg(test)]
use tracing::error;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum FinalizedTargetDisposition {
    Attempt,
    Unchanged,
    Rejected,
}

#[cfg(test)]
pub(in super::super) fn record_finalized_target(
    latest: &mut Option<FinalizedTarget>,
    pending: &mut Option<FinalizedTarget>,
    incoming: FinalizedTarget,
) -> eyre::Result<bool> {
    match *latest {
        Some(current) if incoming.number < current.number => Err(eyre::eyre!(
            "finalized target regressed from {} ({}) to {} ({})",
            current.number,
            current.hash,
            incoming.number,
            incoming.hash
        )),
        Some(current) if incoming.number == current.number && incoming.hash != current.hash => {
            Err(eyre::eyre!(
                "finalized target hash changed at height {}: {} -> {}",
                current.number,
                current.hash,
                incoming.hash
            ))
        }
        Some(current) if incoming == current => Ok(pending.is_some()),
        _ => {
            *latest = Some(incoming);
            *pending = Some(incoming);
            Ok(true)
        }
    }
}

/// During crash recovery Mongo can durably commit finalized block N just before
/// Reth persists its finalized marker for N. Ignore the stale N-1 marker until
/// Reth reaches the already-canonical Mongo checkpoint; never accept a conflict
/// at the checkpoint height.
#[cfg(test)]
pub(in super::super) fn admit_startup_finalized_target(
    startup_floor: &mut Option<FinalizedTarget>,
    incoming: FinalizedTarget,
) -> eyre::Result<bool> {
    let Some(floor) = *startup_floor else {
        return Ok(true);
    };
    if incoming.number < floor.number {
        return Ok(false);
    }
    if incoming.number == floor.number && incoming.hash != floor.hash {
        bail!(
            "finalized target conflicts with recovered projection checkpoint at height {}: {} != {}",
            floor.number,
            incoming.hash,
            floor.hash
        );
    }
    *startup_floor = None;
    Ok(true)
}

#[cfg(test)]
pub(in super::super) fn record_or_publish_finalized_target(
    latest: &mut Option<FinalizedTarget>,
    pending: &mut Option<FinalizedTarget>,
    incoming: FinalizedTarget,
    publisher: &ProjectionReadinessPublisher,
    exit: &tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
) -> FinalizedTargetDisposition {
    match record_finalized_target(latest, pending, incoming) {
        Ok(true) => FinalizedTargetDisposition::Attempt,
        Ok(false) => FinalizedTargetDisposition::Unchanged,
        Err(error) => {
            error!(%error, "rejected unsafe finalized projection target");
            publish_fatal(
                publisher,
                exit,
                ProjectionFailureClass::CheckpointMismatch,
                error.to_string(),
            );
            FinalizedTargetDisposition::Rejected
        }
    }
}
