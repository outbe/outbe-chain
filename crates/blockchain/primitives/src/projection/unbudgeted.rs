use super::{ProjectionCheckpoint, ProjectionFailure, ProjectionReadinessHandle, WaitOutcome};

/// Failure of an exact-parent wait without a caller-provided expiry budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionWaitFailure {
    BudgetExpired,
    ProjectionAhead,
    Fatal(ProjectionFailure),
}

impl ProjectionWaitFailure {
    /// Converts a failure with exactly one caller-owned diagnostic constructor.
    pub fn into_error<E>(
        self,
        budget_expired: impl FnOnce() -> E,
        projection_ahead: impl FnOnce() -> E,
        fatal: impl FnOnce(ProjectionFailure) -> E,
    ) -> E {
        match self {
            Self::BudgetExpired => budget_expired(),
            Self::ProjectionAhead => projection_ahead(),
            Self::Fatal(failure) => fatal(failure),
        }
    }
}

impl ProjectionReadinessHandle {
    /// Waits without an expiry budget, retaining the readiness channel's failure semantics.
    pub async fn wait_without_budget(
        self,
        required: ProjectionCheckpoint,
    ) -> Result<(), ProjectionWaitFailure> {
        match self.wait_for(required, std::future::pending()).await {
            WaitOutcome::Ready => Ok(()),
            WaitOutcome::BudgetExpired => Err(ProjectionWaitFailure::BudgetExpired),
            WaitOutcome::ProjectionAhead => Err(ProjectionWaitFailure::ProjectionAhead),
            WaitOutcome::Fatal(failure) => Err(ProjectionWaitFailure::Fatal(failure)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::{projection_readiness, ProjectionFailureClass, ProjectionStatus};
    use alloy_primitives::B256;

    fn checkpoint(number: u64) -> ProjectionCheckpoint {
        ProjectionCheckpoint {
            block_number: number,
            block_hash: B256::with_last_byte(number as u8),
        }
    }

    #[test]
    fn error_conversion_invokes_only_the_selected_handler() {
        assert_eq!(
            ProjectionWaitFailure::BudgetExpired.into_error(
                || "budget",
                || panic!("unexpected ahead handler"),
                |_| panic!("unexpected fatal handler"),
            ),
            "budget"
        );
        assert_eq!(
            ProjectionWaitFailure::ProjectionAhead.into_error(
                || panic!("unexpected budget handler"),
                || "ahead",
                |_| panic!("unexpected fatal handler"),
            ),
            "ahead"
        );
        let expected =
            ProjectionFailure::new(ProjectionFailureClass::CorruptBody, "original failure");
        let actual = ProjectionWaitFailure::Fatal(expected.clone()).into_error(
            || panic!("unexpected budget handler"),
            || panic!("unexpected ahead handler"),
            |failure| failure,
        );
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn exact_ready_checkpoint_succeeds() {
        let required = checkpoint(2);
        let (_publisher, readiness) = projection_readiness(
            checkpoint(0),
            ProjectionStatus::Ready {
                checkpoint: required,
            },
        );
        assert_eq!(readiness.wait_without_budget(required).await, Ok(()));
    }

    #[tokio::test]
    async fn projection_ahead_remains_a_distinct_failure() {
        let (_publisher, readiness) = projection_readiness(
            checkpoint(0),
            ProjectionStatus::Ready {
                checkpoint: checkpoint(3),
            },
        );
        assert_eq!(
            readiness.wait_without_budget(checkpoint(2)).await,
            Err(ProjectionWaitFailure::ProjectionAhead)
        );
    }

    #[tokio::test]
    async fn fatal_failure_retains_class_and_message() {
        let error =
            ProjectionFailure::new(ProjectionFailureClass::CorruptBody, "corrupt fixture body");
        let (_publisher, readiness) = projection_readiness(
            checkpoint(0),
            ProjectionStatus::Fatal {
                checkpoint: None,
                error: error.clone(),
            },
        );
        assert_eq!(
            readiness.wait_without_budget(checkpoint(2)).await,
            Err(ProjectionWaitFailure::Fatal(error))
        );
    }

    #[tokio::test]
    async fn dropped_publisher_retains_channel_closed_failure() {
        let (publisher, readiness) =
            projection_readiness(checkpoint(0), ProjectionStatus::Starting);
        drop(publisher);
        match readiness.wait_without_budget(checkpoint(2)).await {
            Err(ProjectionWaitFailure::Fatal(error)) => {
                assert_eq!(error.class, ProjectionFailureClass::ReadinessChannelClosed);
            }
            other => panic!("expected channel closure failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_wait_completes_after_exact_checkpoint_is_published() {
        let required = checkpoint(2);
        let (publisher, readiness) = projection_readiness(
            checkpoint(0),
            ProjectionStatus::CatchingUp {
                checkpoint: Some(checkpoint(1)),
            },
        );
        let wait = readiness.wait_without_budget(required);
        tokio::pin!(wait);
        tokio::select! {
            biased;
            result = &mut wait => panic!("wait completed before readiness: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        publisher.publish(ProjectionStatus::Ready {
            checkpoint: required,
        });
        assert_eq!(wait.await, Ok(()));
    }
}
