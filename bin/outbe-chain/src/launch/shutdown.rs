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

// Manager and endpoint each have a five-second drain budget. Leave time for
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
/// join from `Drop` as the ordinary completion path. This prevents Reth's ExEx and
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

pub(crate) fn record_ocomp_exit(
    exit: Option<crate::ocomp_exex::OcompExExExitV1>,
    shutdown: &outbe_node::shutdown::NodeShutdown,
) {
    if let Some(exit) = exit {
        tracing::error!(
            failure_class = ?exit.failure.class,
            failure = %exit.failure.message,
            "embedded OCOMP requested node shutdown"
        );
        shutdown.record_failure(eyre::eyre!(
            "embedded OCOMP failure ({:?}): {}",
            exit.failure.class,
            exit.failure.message
        ));
    } else {
        shutdown.record_failure(eyre::eyre!(
            "embedded OCOMP exit channel closed without a verdict"
        ));
    }
}

pub(crate) async fn wait_for_launcher_exit(
    shutdown: &outbe_node::shutdown::NodeShutdown,
    consensus_dead_rx: Option<&mut oneshot::Receiver<()>>,
    ocomp_exit_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::ocomp_exex::OcompExExExitV1>,
    upgrade_promotion: &tokio::sync::Notify,
    tee_lease_exit_rx: &mut tokio::sync::mpsc::UnboundedReceiver<eyre::Result<String>>,
) -> LauncherExitCause {
    let has_consensus = consensus_dead_rx.is_some();
    let consensus_dead = async {
        match consensus_dead_rx {
            Some(rx) => {
                let _ = rx.await;
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        () = shutdown.engine_exited() => {
            info!("execution node exited");
            LauncherExitCause::NodeExited
        }
        () = consensus_dead => {
            info!("consensus node exited");
            LauncherExitCause::ConsensusExited
        }
        exit = ocomp_exit_rx.recv() => {
            record_ocomp_exit(exit, shutdown);
            LauncherExitCause::OcompRequested
        }
        () = upgrade_promotion.notified() => {
            info!("finalized enclave upgrade requested execution restart");
            LauncherExitCause::UpgradeRequested
        }
        rejection = tee_lease_exit_rx.recv() => {
            let reason = tee_lease_exit_reason(rejection, shutdown);
            if has_consensus {
                tracing::error!(reason = %reason, "finalized TEE lease guard requested node shutdown");
            } else {
                tracing::error!(reason = %reason, "finalized TEE lease guard requested full-node shutdown");
            }
            LauncherExitCause::TeeLeaseRejected
        }
        signal = tokio::signal::ctrl_c() => {
            if let Err(error) = signal {
                shutdown.record_failure(eyre::eyre!(error).wrap_err("shutdown signal listener failed"));
            }
            info!("received shutdown signal");
            LauncherExitCause::CtrlC
        }
    }
}

pub(crate) fn join_consensus_thread(
    consensus_lifecycle: ConsensusThreadGuard,
    shutdown: &outbe_node::shutdown::NodeShutdown,
) {
    let consensus_joined = consensus_lifecycle.join();
    tracing::debug!("consensus thread join completed");
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_consensus_thread_join(consensus_joined)
    })) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => shutdown.record_failure(error),
        Err(panic) => shutdown.record_panic(panic),
    }
}

pub(crate) struct LauncherDrainTasks {
    pub(crate) tee_lease_guard_handle: tokio::task::JoinHandle<()>,
    pub(crate) radicle_observer: Option<tokio::task::JoinHandle<()>>,
    pub(crate) tee_canary_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) txpool_maintenance_handle: tokio::task::JoinHandle<()>,
    pub(crate) upgrade_handle: Option<tokio::task::JoinHandle<()>>,
}

async fn abort_and_record(
    handle: tokio::task::JoinHandle<()>,
    label: &str,
    shutdown: &outbe_node::shutdown::NodeShutdown,
) {
    handle.abort();
    match handle.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => {
            shutdown.record_failure(eyre::eyre!("{label} panicked: {error}"));
        }
    }
}

async fn drain_radicle_observer(
    mut handle: tokio::task::JoinHandle<()>,
    shutdown: &outbe_node::shutdown::NodeShutdown,
) {
    match tokio::time::timeout(RADICLE_DRAIN_DEADLINE, &mut handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            shutdown.record_failure(eyre::eyre!(error).wrap_err("Radicle observer panicked"))
        }
        Err(error) => {
            tracing::warn!("Radicle observer join deadline exceeded");
            shutdown.record_failure(
                eyre::eyre!(error).wrap_err("Radicle observer join deadline exceeded"),
            );
            handle.abort();
            if let Err(error) = handle.await {
                if !error.is_cancelled() {
                    shutdown.record_failure(
                        eyre::eyre!(error).wrap_err("Radicle observer failed while reaping"),
                    );
                }
            }
        }
    }
}

pub(crate) async fn drain_launcher_tasks(
    shutdown_token: &tokio_util::sync::CancellationToken,
    shutdown: &outbe_node::shutdown::NodeShutdown,
    tasks: LauncherDrainTasks,
) {
    shutdown_token.cancel();
    if let Err(error) = tasks.tee_lease_guard_handle.await {
        shutdown.record_failure(eyre::eyre!(error).wrap_err("TEE lease guard panicked"));
    }
    if let Some(handle) = tasks.radicle_observer {
        drain_radicle_observer(handle, shutdown).await;
    }
    if let Some(handle) = tasks.tee_canary_handle {
        tracing::debug!("waiting for TEE canary worker shutdown");
        if let Err(error) = handle.await {
            shutdown.record_failure(eyre::eyre!(error).wrap_err("TEE canary worker panicked"));
        }
        tracing::debug!("TEE canary worker shutdown completed");
    }
    abort_and_record(
        tasks.txpool_maintenance_handle,
        "txpool maintenance task",
        shutdown,
    )
    .await;
    if let Some(handle) = tasks.upgrade_handle {
        abort_and_record(handle, "enclave-upgrade watcher", shutdown).await;
    }
}

