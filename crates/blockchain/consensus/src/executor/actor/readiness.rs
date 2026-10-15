use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionReadinessHandle;

/// Whether to update head or finalized.
#[derive(Debug)]
pub(super) enum HeadOrFinalized {
    Head,
    Finalized,
}

pub(super) async fn wait_for_finalized_parent(
    readiness: ProjectionReadinessHandle,
    required: ProjectionCheckpoint,
) -> eyre::Result<()> {
    readiness
        .wait_without_budget(required)
        .await
        .map_err(|failure| {
            failure.into_error(
                || crate::errors::ExecutorError::FinalizedParentWaitExpired.into(),
                || {
                    crate::errors::ExecutorError::ProjectionAheadOfFinalizedParent {
                        block_hash: required.block_hash,
                        block_number: required.block_number,
                    }
                    .into()
                },
                |failure| {
                    crate::errors::ExecutorError::ProjectionReadinessFailed {
                        class: failure.class,
                        message: failure.message,
                    }
                    .into()
                },
            )
        })
}

pub(super) async fn wait_for_optional_ocomp_parent(
    readiness: Option<ProjectionReadinessHandle>,
    required: ProjectionCheckpoint,
) -> eyre::Result<()> {
    let Some(readiness) = readiness else {
        return Ok(());
    };
    wait_for_finalized_parent(readiness, required)
        .await
        .map_err(|error| crate::errors::ExecutorError::FullNodeOcompReadinessFailed(error).into())
}
