use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionReadinessHandle;

/// Whether to update head or finalized.
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
                || eyre::eyre!("finalized parent projection wait expired without a request budget"),
                || {
                    eyre::eyre!(
                        "projection is ahead of finalized parent {} at height {}",
                        required.block_hash,
                        required.block_number
                    )
                },
                |failure| {
                    eyre::eyre!(
                        "projection readiness failed ({:?}): {}",
                        failure.class,
                        failure.message
                    )
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
        .map_err(|error| eyre::eyre!("FullNode OCOMP readiness failed: {error}"))
}
