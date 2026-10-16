use super::*;

#[derive(Debug)]
pub(in crate::stack) enum EpochLoopOutcome {
    RestartEpoch,
    ReplaceSigner,
    GlobalStop,
    EngineExit(Result<(), commonware_runtime::Error>),
    StackExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::stack) enum EpochLoopAction {
    RestartEpoch,
    ReplaceSigner,
    ExitStack,
}

const STACK_ENGINE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

async fn drain_engine_with_deadline<E>(
    ctx: &E,
    engine: &mut commonware_runtime::Handle<()>,
) -> Result<()>
where
    E: Clock,
{
    let deadline = ctx.sleep(STACK_ENGINE_DRAIN_TIMEOUT);
    tokio::pin!(deadline);

    tokio::select! {
        biased;
        engine_result = &mut *engine => {
            engine_result.map_err(|error| {
                crate::errors::StackShutdownError::SimplexEngineFailedWhileDraining(format!("{error:?}")).into()
            })
        }
        _ = &mut deadline => {
            engine.abort();
            let _ = (&mut *engine).await;
            Err(crate::errors::StackShutdownError::SimplexEngineDrainTimedOut(
                STACK_ENGINE_DRAIN_TIMEOUT,
            )
            .into())
        }
    }
}

async fn signal_global_stop_and_drain_engine<E>(
    ctx: &E,
    engine: &mut commonware_runtime::Handle<()>,
) -> Result<()>
where
    E: Clock + Spawner,
{
    let stop_handle = ctx
        .child("terminal_stack_stop")
        .spawn(|shutdown| async move { shutdown.stop(0, Some(STACK_ENGINE_DRAIN_TIMEOUT)).await });
    let engine_result = drain_engine_with_deadline(ctx, engine).await;

    let stop_result = stop_handle
        .await
        .map_err(|error| {
            crate::errors::StackShutdownError::TerminalStackStopTaskFailed(format!("{error:?}"))
        })?
        .map_err(|error| -> eyre::Report {
            crate::errors::StackShutdownError::TerminalStackStopFailed(format!("{error:?}")).into()
        });

    match (engine_result, stop_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(engine_error), Err(stop_error)) => Err(
            crate::errors::StackShutdownError::EngineDrainAndTerminalStopFailed {
                engine_error: format!("{engine_error:#}"),
                stop_error: format!("{stop_error:#}"),
            }
            .into(),
        ),
    }
}

async fn signal_global_stop_after_engine_exit<E>(ctx: &E) -> Result<()>
where
    E: Clock + Spawner,
{
    ctx.child("terminal_stack_stop")
        .stop(0, Some(STACK_ENGINE_DRAIN_TIMEOUT))
        .await
        .map_err(|error| {
            crate::errors::StackShutdownError::TerminalStackStopFailed(format!("{error:?}")).into()
        })
}

pub(in crate::stack) fn preserve_stack_result_after_drain<T>(
    result: Result<T>,
    drain_result: Result<()>,
) -> Result<T> {
    match (result, drain_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(drain_error)) => Err(drain_error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(drain_error)) => Err(error.wrap_err(
            crate::errors::StackShutdownError::additional_drain_failure(&drain_error),
        )),
    }
}

pub(in crate::stack) async fn supervise_epoch_loop_result<E>(
    ctx: &E,
    outcome: Result<EpochLoopOutcome>,
    engine: &mut commonware_runtime::Handle<()>,
    application: &crate::application_shutdown::ApplicationDrain,
) -> Result<EpochLoopAction>
where
    E: Clock + Spawner,
{
    if matches!(
        &outcome,
        Err(_) | Ok(EpochLoopOutcome::EngineExit(_) | EpochLoopOutcome::StackExit)
    ) {
        // The outer stack owner retains and reports this shared drain result.
        // Even on failure, finish its bounded cleanup before stopping transport.
        let _ = application.drain().await;
    }
    match outcome {
        Ok(EpochLoopOutcome::RestartEpoch) => Ok(EpochLoopAction::RestartEpoch),
        Ok(EpochLoopOutcome::ReplaceSigner) => {
            engine.abort();
            let _ = (&mut *engine).await;
            Ok(EpochLoopAction::ReplaceSigner)
        }
        Ok(EpochLoopOutcome::GlobalStop) => {
            drain_engine_with_deadline(ctx, engine).await?;
            Ok(EpochLoopAction::ExitStack)
        }
        Ok(EpochLoopOutcome::EngineExit(engine_result)) => {
            let engine_result =
                engine_result
                    .map(|()| EpochLoopAction::ExitStack)
                    .map_err(|error| {
                        crate::errors::StackShutdownError::SimplexEngineExited(format!("{error:?}"))
                            .into()
                    });
            preserve_stack_result_after_drain(
                engine_result,
                signal_global_stop_after_engine_exit(ctx).await,
            )
        }
        Ok(EpochLoopOutcome::StackExit) => preserve_stack_result_after_drain(
            Ok(EpochLoopAction::ExitStack),
            signal_global_stop_and_drain_engine(ctx, engine).await,
        ),
        Err(error) => preserve_stack_result_after_drain(
            Err(error),
            signal_global_stop_and_drain_engine(ctx, engine).await,
        ),
    }
}
