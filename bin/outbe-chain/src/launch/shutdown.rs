use crate::*;

pub(crate) fn handle_consensus_thread_join(
    joined: thread::Result<eyre::Result<()>>,
) -> eyre::Result<()> {
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err.wrap_err("consensus task exited with error")),
        Err(unwind) => std::panic::resume_unwind(unwind),
    }
}

pub(crate) fn run_with_lifetime_pin<P, F, T>(pin: P, run: F) -> T
where
    F: FnOnce() -> T,
{
    let output = run();
    drop(pin);
    output
}

// Manager and endpoint each have a five-second drain budget; leave time for
// acknowledgement and task reaping before shutting down their transport.
pub(crate) const RADICLE_DRAIN_DEADLINE: Duration = Duration::from_secs(12);

pub(crate) async fn await_radicle_drain(
    completion: Option<oneshot::Receiver<()>>,
    deadline: Duration,
) -> eyre::Result<()> {
    let Some(completion) = completion else {
        return Ok(());
    };
    tokio::time::timeout(deadline, completion)
        .await
        .map_err(|_| eyre::eyre!("Radicle drain deadline exceeded before transport shutdown"))?
        .map_err(|_| eyre::eyre!("Radicle observer exited without completing its drain"))
}

pub(crate) async fn abort_and_wait_supervised<T>(
    handle: &mut commonware_runtime::Handle<T>,
) -> Result<Option<T>, commonware_runtime::Error>
where
    T: Send + 'static,
{
    handle.abort();
    match handle.await {
        Ok(result) => Ok(Some(result)),
        // Aborting an unfinished supervised task closes its result channel.
        Err(commonware_runtime::Error::Closed) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Global stop acknowledges signal guards, not publication of the stack result.
/// Give the retained owner time to return its terminal error before aborting it.
pub(crate) async fn await_consensus_stack_shutdown(
    handle: &mut commonware_runtime::Handle<eyre::Result<()>>,
    deadline: Duration,
) -> eyre::Result<()> {
    match tokio::time::timeout(deadline, &mut *handle).await {
        Ok(result) => result.map_err(|error| {
            eyre::eyre!("consensus stack task failed during shutdown: {error:?}")
        })?,
        Err(_) => {
            let timeout = eyre::eyre!("consensus stack result deadline exceeded after global stop");
            match abort_and_wait_supervised(handle).await {
                Ok(Some(Err(error))) => Err(error.wrap_err(format!("{timeout:#}"))),
                Err(error) => Err(timeout.wrap_err(format!(
                    "consensus stack task failed while reaping: {error:?}"
                ))),
                // Forced cancellation is cleanup, not a successful stack exit.
                Ok(Some(Ok(())) | None) => Err(timeout),
            }
        }
    }
}

pub(crate) fn consensus_shutdown_result(
    stop: Result<(), commonware_runtime::Error>,
    stack: eyre::Result<()>,
) -> eyre::Result<()> {
    let stop = stop.map_err(|error| {
        eyre::eyre!("consensus graceful shutdown did not complete within 5 seconds: {error}")
    });
    match (stop, stack) {
        (Err(stop), Err(stack)) => Err(stack.wrap_err(format!("{stop:#}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LauncherExitCause {
    NodeExited,
    ConsensusExited,
    OcompRequested,
    UpgradeRequested,
    TeeLeaseRejected,
    CtrlC,
}

/// Keeps the Commonware runtime alive until its task tree has observed shutdown.
///
/// Reth owns the process signal handler and may cancel the complete node launcher
/// future. This guard therefore performs the same cancellation and synchronous
/// join from `Drop` as the ordinary completion path, preventing Reth's ExEx and
/// Engine resources from disappearing while consensus still holds an exact
/// application acknowledgement.
pub(crate) struct ConsensusThreadGuard {
    shutdown: tokio_util::sync::CancellationToken,
    handle: Option<thread::JoinHandle<eyre::Result<()>>>,
    outcome: Option<outbe_node::shutdown::NodeShutdown>,
}

impl ConsensusThreadGuard {
    pub(crate) fn new(
        shutdown: tokio_util::sync::CancellationToken,
        handle: thread::JoinHandle<eyre::Result<()>>,
    ) -> Self {
        Self {
            shutdown,
            handle: Some(handle),
            outcome: None,
        }
    }

    pub(crate) fn with_outcome(mut self, outcome: outbe_node::shutdown::NodeShutdown) -> Self {
        self.outcome = Some(outcome);
        self
    }

    pub(crate) fn join(mut self) -> thread::Result<eyre::Result<()>> {
        self.shutdown.cancel();
        let handle = self
            .handle
            .take()
            .expect("consensus thread handle is consumed exactly once");
        handle.join()
    }
}

impl Drop for ConsensusThreadGuard {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        self.shutdown.cancel();
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "consensus task failed during launcher teardown");
                if let Some(outcome) = &self.outcome {
                    outcome.record_failure(error);
                }
            }
            Err(panic) => {
                tracing::error!("consensus task panicked during launcher teardown");
                if let Some(outcome) = &self.outcome {
                    outcome.record_panic(panic);
                }
            }
        }
    }
}
