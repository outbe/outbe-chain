//! Shared cancellable scheduler; tests and runtime use these exact loops.
use super::engine::Step;
use alloy_primitives::B256;
use std::{future::Future, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct PreparedAnchor {
    pub(crate) height: u64,
    pub(crate) hash: B256,
}

pub(crate) trait LifecycleTask: Send {
    type Error: std::error::Error + Send + Sync + 'static;
    fn is_retryable(error: &Self::Error) -> bool;
    fn tick(&mut self) -> impl Future<Output = Result<Step, Self::Error>> + Send;
    fn prepared(&mut self, step: &Step) -> Result<Option<PreparedAnchor>, Self::Error> {
        Ok(match step {
            Step::Enrollment(outbe_tee::lifecycle::enrollment::EnrollmentOutcome::Admitted {
                anchor,
            }) => Some(PreparedAnchor {
                height: anchor.finalized_height,
                hash: anchor.finalized_hash,
            }),
            _ => None,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PrepareError<E: std::error::Error + 'static> {
    #[error("TEE pre-sync onboarding cancelled")]
    Cancelled,
    #[error("TEE pre-sync lifecycle failed")]
    Observation(#[source] E),
}

// Same cadence as the authority guard; finalization wakes background work sooner.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
async fn wait(cancellation: &CancellationToken, finalized: &tokio::sync::Notify) {
    tokio::select! {
        _ = cancellation.cancelled() => {},
        _ = finalized.notified() => {},
        _ = tokio::time::sleep(POLL_INTERVAL) => {},
    }
}

pub(crate) async fn prepare<T: LifecycleTask>(
    task: &mut T,
    cancellation: &CancellationToken,
    finalized: &tokio::sync::Notify,
) -> Result<Option<PreparedAnchor>, PrepareError<T::Error>> {
    loop {
        let step = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(PrepareError::Cancelled),
            result = task.tick() => result,
        };
        let step = match step {
            Ok(step) => step,
            Err(error) if T::is_retryable(&error) => {
                tracing::warn!(%error, "TEE pre-sync lifecycle observation failed; retrying");
                wait(cancellation, finalized).await;
                continue;
            }
            Err(error) => return Err(PrepareError::Observation(error)),
        };
        if matches!(step, Step::AwaitingNetwork) {
            return Ok(None);
        }
        if let Step::Resident { at, jailed, .. } = step {
            tracing::info!(
                height = at.height,
                hash = %at.hash,
                jailed,
                "TEE pre-sync verified resident identity"
            );
            return Ok(Some(PreparedAnchor {
                height: at.height,
                hash: at.hash,
            }));
        }
        if let Some(anchor) = task.prepared(&step).map_err(PrepareError::Observation)? {
            return Ok(Some(anchor));
        }
        tracing::info!(reason = ?step, "TEE pre-sync onboarding waiting");
        wait(cancellation, finalized).await;
    }
}

pub(crate) async fn run<T: LifecycleTask>(
    mut task: T,
    cancellation: CancellationToken,
    finalized: Arc<tokio::sync::Notify>,
) {
    loop {
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return,
            result = task.tick() => result,
        };
        match result {
            Ok(step) => match step {
                Step::Renewal(outcome)
                | Step::Resident {
                    renewal: Some(outcome),
                    ..
                } => tracing::debug!(?outcome, "TEE renewal observation"),
                step => tracing::debug!(reason=?step, "TEE lifecycle observation"),
            },
            // Keep resident keys and durable intent across transient chain I/O.
            Err(error) => tracing::warn!(%error, "TEE lifecycle observation failed"),
        }
        wait(&cancellation, &finalized).await;
    }
}
