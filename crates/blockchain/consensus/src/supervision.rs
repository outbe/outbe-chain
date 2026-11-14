//! Fatal actor exit reporting with typed causes and structured error chains.

use outbe_primitives::telemetry::error_field;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SupervisedActorError {
    #[error("consensus actor `{actor}` exited unexpectedly")]
    UnexpectedExit { actor: &'static str },
    #[error("consensus actor `{actor}` task failed")]
    TaskFailed {
        actor: &'static str,
        #[source]
        source: commonware_runtime::Error,
    },
}

pub(crate) fn unexpected_actor_exit(
    actor: &'static str,
    outcome: Result<(), commonware_runtime::Error>,
) -> SupervisedActorError {
    let err = match outcome {
        Ok(()) => SupervisedActorError::UnexpectedExit { actor },
        Err(source) => SupervisedActorError::TaskFailed { actor, source },
    };
    tracing::error!(actor, error = error_field(&err), "consensus actor exited");
    err
}

pub(crate) fn unexpected_fallible_actor_exit<E: core::error::Error + Send + Sync + 'static>(
    actor: &'static str,
    outcome: Result<Result<(), E>, commonware_runtime::Error>,
) -> eyre::Report {
    match outcome {
        Ok(Err(err)) => {
            tracing::error!(actor, error = error_field(&err), "consensus actor failed");
            eyre::Report::new(err)
                .wrap_err(format!("consensus actor `{actor}` returned fatal error"))
        }
        outcome => unexpected_actor_exit(actor, outcome.map(|_| ())).into(),
    }
}

pub(crate) fn unexpected_eyre_actor_exit(
    actor: &'static str,
    outcome: Result<eyre::Result<()>, commonware_runtime::Error>,
) -> eyre::Report {
    match outcome {
        Ok(Err(report)) => {
            tracing::error!(
                actor,
                error = error_field::<dyn core::error::Error>(report.as_ref()),
                "consensus actor failed"
            );
            report.wrap_err(format!("consensus actor `{actor}` returned fatal error"))
        }
        outcome => unexpected_actor_exit(actor, outcome.map(|_| ())).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::error::Error as _;

    #[derive(Debug, thiserror::Error)]
    #[error("actor failure")]
    struct ActorFailure(#[source] std::io::Error);

    #[test]
    fn clean_exit_is_unexpected() {
        assert!(matches!(
            unexpected_actor_exit("test_actor", Ok(())),
            SupervisedActorError::UnexpectedExit {
                actor: "test_actor"
            }
        ));
    }

    #[test]
    fn runtime_failure_preserves_typed_source() {
        let error = unexpected_actor_exit("test_actor", Err(commonware_runtime::Error::Exited));
        assert!(matches!(
            error,
            SupervisedActorError::TaskFailed {
                actor: "test_actor",
                ..
            }
        ));
        assert!(matches!(
            error.source().unwrap().downcast_ref(),
            Some(commonware_runtime::Error::Exited)
        ));
    }

    #[test]
    fn fallible_exit_preserves_nested_error() {
        let error = unexpected_fallible_actor_exit(
            "finalization_actor",
            Ok(Err(ActorFailure(std::io::Error::other("root")))),
        );
        assert_eq!(
            error.to_string(),
            "consensus actor `finalization_actor` returned fatal error"
        );
        assert!(error
            .downcast_ref::<ActorFailure>()
            .unwrap()
            .source()
            .unwrap()
            .is::<std::io::Error>());
        assert!(error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()));
    }

    #[test]
    fn eyre_exit_preserves_report_context_and_typed_cause() {
        let report = eyre::Report::new(ActorFailure(std::io::Error::other("root")))
            .wrap_err("executor context");
        let error = unexpected_eyre_actor_exit("executor", Ok(Err(report)));
        assert_eq!(
            error.to_string(),
            "consensus actor `executor` returned fatal error"
        );
        assert_eq!(
            error.chain().nth(1).unwrap().to_string(),
            "executor context"
        );
        assert!(error
            .downcast_ref::<ActorFailure>()
            .unwrap()
            .source()
            .unwrap()
            .is::<std::io::Error>());
        assert!(error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some()));
    }

    #[test]
    fn fallible_and_eyre_runtime_exits_preserve_supervision_error() {
        for outcome in [Ok(Ok(())), Err(commonware_runtime::Error::Closed)] {
            let error =
                unexpected_fallible_actor_exit::<ActorFailure>("finalization_actor", outcome);
            assert!(error.downcast_ref::<SupervisedActorError>().is_some());
        }
        for outcome in [Ok(Ok(())), Err(commonware_runtime::Error::Closed)] {
            let error = unexpected_eyre_actor_exit("executor", outcome);
            assert!(error.downcast_ref::<SupervisedActorError>().is_some());
        }
    }
}
