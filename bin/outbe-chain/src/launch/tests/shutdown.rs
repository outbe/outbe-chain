use super::*;

#[tokio::test]
async fn radicle_transport_waits_for_drain_completion() {
    use futures::FutureExt as _;
    let (done, completion) = tokio::sync::oneshot::channel();
    let waiting = super::await_radicle_drain(Some(completion), std::time::Duration::from_secs(1));
    tokio::pin!(waiting);
    assert!(waiting.as_mut().now_or_never().is_none());
    done.send(()).unwrap();
    waiting.await.unwrap();
    super::await_radicle_drain(None, std::time::Duration::ZERO)
        .await
        .unwrap();
}

#[tokio::test]
async fn radicle_lost_observer_is_not_a_clean_drain() {
    let (done, completion) = tokio::sync::oneshot::channel();
    drop(done);
    let error = super::await_radicle_drain(Some(completion), std::time::Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("without completing its drain"));
}

#[tokio::test]
async fn radicle_drain_deadline_is_distinct_from_lost_observer() {
    let (_done, completion) = tokio::sync::oneshot::channel();
    let error = super::await_radicle_drain(Some(completion), std::time::Duration::ZERO)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("deadline exceeded"));
}

#[test]
fn dropped_launcher_joins_consensus_before_execution_teardown() {
    let consensus_stopped = Arc::new(AtomicBool::new(false));
    let execution = ExecutionTeardownSentinel(Arc::clone(&consensus_stopped));
    let shutdown = tokio_util::sync::CancellationToken::new();
    let worker_shutdown = shutdown.clone();
    let worker_stopped = Arc::clone(&consensus_stopped);
    let worker = std::thread::spawn(move || {
        while !worker_shutdown.is_cancelled() {
            std::thread::yield_now();
        }
        worker_stopped.store(true, Ordering::SeqCst);
        Ok(())
    });

    let lifecycle = super::ConsensusThreadGuard::new(shutdown, worker);
    drop(lifecycle);

    assert!(consensus_stopped.load(Ordering::SeqCst));
    drop(execution);
}

#[test]
fn node_pin_drops_after_runner_on_consensus_thread() {
    let runner_returned = Arc::new(AtomicBool::new(false));
    let dropped_on = Arc::new(Mutex::new(None));
    let expected_thread = std::thread::current().id();
    let pin = Arc::new(ThreadDropRecorder {
        runner_returned: Arc::clone(&runner_returned),
        dropped_on: Arc::clone(&dropped_on),
    });
    let worker_pin = Arc::clone(&pin);

    let output = super::run_with_lifetime_pin(pin, || {
        std::thread::spawn(move || drop(worker_pin))
            .join()
            .expect("worker exits cleanly");
        runner_returned.store(true, Ordering::SeqCst);
        7
    });

    assert_eq!(output, 7);
    assert_eq!(
        *dropped_on.lock().expect("drop recorder lock"),
        Some(expected_thread)
    );
}

#[test]
fn supervised_shutdown_waits_for_descendants() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};

    let dropped = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&dropped);
    commonware_runtime::tokio::Runner::default().start(async move |ctx| {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let child_dropped = Arc::clone(&observed);
        let mut stack = ctx.child("test_stack").spawn(move |_| async move {
            struct ChildDrop(Arc<AtomicBool>);
            impl Drop for ChildDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }

            let _drop = ChildDrop(child_dropped);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("child started");

        assert!(super::abort_and_wait_supervised(&mut stack)
            .await
            .unwrap()
            .is_none());
        assert!(observed.load(Ordering::SeqCst));
    });
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn cancellation_guard_preserves_consensus_error_after_launcher_drop() {
    let outcome = outbe_node::shutdown::NodeShutdown::default();
    let worker = std::thread::spawn(|| Err(eyre::eyre!("consensus drain failed")));
    let guard =
        super::ConsensusThreadGuard::new(tokio_util::sync::CancellationToken::new(), worker)
            .with_outcome(outcome.clone());
    drop(guard);
    let error = outcome
        .finish(Ok(()))
        .expect_err("cancelled launcher must retain failure");
    assert!(format!("{error:#}").contains("consensus drain failed"));
}

#[test]
fn supervised_shutdown_preserves_an_already_completed_failure() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    let config = commonware_runtime::tokio::Config::default().with_worker_threads(1);
    commonware_runtime::tokio::Runner::new(config).start(async move |ctx| {
        let (completed, wait) = tokio::sync::oneshot::channel();
        let mut stack = ctx.child("failed_stack").spawn(move |_| async move {
            completed.send(()).unwrap();
            Err::<(), _>(eyre::eyre!("stack failure during drain"))
        });
        // On this one-worker runtime, the non-yielding child returns its
        // result before this receiver can resume. No wall-clock sleeps.
        wait.await.unwrap();
        let result = super::abort_and_wait_supervised(&mut stack).await.unwrap();
        let error = result
            .expect("completed task result must survive abort")
            .unwrap_err();
        assert!(format!("{error:#}").contains("stack failure during drain"));
    });
}

#[test]
fn supervised_shutdown_waits_for_a_pending_terminal_result() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    use futures::FutureExt as _;
    commonware_runtime::tokio::Runner::default().start(async move |ctx| {
        let (release, ready) = tokio::sync::oneshot::channel();
        let mut stack = ctx.child("terminal_result").spawn(move |_| async move {
            ready.await.unwrap();
            Err(eyre::eyre!("original terminal error after drain"))
        });
        let wait =
            super::await_consensus_stack_shutdown(&mut stack, std::time::Duration::from_secs(1));
        tokio::pin!(wait);
        assert!(
            wait.as_mut().now_or_never().is_none(),
            "shutdown must not abort a pending result"
        );
        release.send(()).unwrap();
        assert!(format!("{:#}", wait.await.unwrap_err())
            .contains("original terminal error after drain"));
    });
}

#[test]
fn supervised_shutdown_reaps_a_stalled_stack_but_returns_failure() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::tokio::Runner::default().start(async move |ctx| {
        let (alive, dropped) = tokio::sync::oneshot::channel::<()>();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut stack = ctx
            .child("stalled_terminal_result")
            .spawn(move |_| async move {
                let _alive = alive;
                started.send(()).unwrap();
                std::future::pending::<eyre::Result<()>>().await
            });
        ready.await.unwrap();
        let error = super::await_consensus_stack_shutdown(&mut stack, std::time::Duration::ZERO)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("result deadline exceeded"));
        assert!(
            dropped.await.is_err(),
            "forced cancellation must reap the stack"
        );
    });
}

#[tokio::test]
async fn application_drain_does_not_trigger_the_external_signal_branch() {
    let external = tokio_util::sync::CancellationToken::new();
    let application = external.child_token();
    application.cancel();
    assert!(!external.is_cancelled());
    let another_application = external.child_token();
    external.cancel();
    assert!(another_application.is_cancelled());
}

#[test]
fn consensus_shutdown_preserves_both_timeout_and_stack_error() {
    let error = super::consensus_shutdown_result(
        Err(commonware_runtime::Error::Timeout),
        Err(eyre::eyre!("failed finalization during drain")),
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("did not complete within 5 seconds"));
    assert!(message.contains("failed finalization during drain"));
}

#[test]
fn cancellation_guard_preserves_consensus_panic_after_launcher_drop() {
    let outcome = outbe_node::shutdown::NodeShutdown::default();
    let worker = std::thread::spawn(|| -> eyre::Result<()> { panic!("consensus panic marker") });
    let guard =
        super::ConsensusThreadGuard::new(tokio_util::sync::CancellationToken::new(), worker)
            .with_outcome(outcome.clone());
    drop(guard);
    let error = outcome
        .finish(Ok(()))
        .expect_err("cancelled launcher must retain panic");
    assert!(format!("{error:#}").contains("consensus panic marker"));
}

#[test]
fn explicit_consensus_finish_preserves_error() {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let worker = std::thread::spawn(|| Err(eyre::eyre!("consensus failed")));
    let lifecycle = super::ConsensusThreadGuard::new(shutdown, worker);

    let error = super::handle_consensus_thread_join(lifecycle.join())
        .expect_err("consensus error must propagate through explicit finish");
    assert!(format!("{error:#}").contains("consensus failed"));
}

#[test]
fn explicit_consensus_finish_preserves_panic() {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let worker = std::thread::spawn(|| -> eyre::Result<()> {
        panic!("consensus panicked");
    });
    let lifecycle = super::ConsensusThreadGuard::new(shutdown, worker);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::handle_consensus_thread_join(lifecycle.join())
    }));
    assert!(
        panic.is_err(),
        "consensus panic must resume on explicit finish"
    );
}

#[test]
fn consensus_thread_error_propagates_to_validator_main() {
    let err = super::handle_consensus_thread_join(Ok(Err(eyre::eyre!("watchdog fatal"))))
        .expect_err("consensus thread error must propagate");
    let err = format!("{err:#}");

    assert!(
        err.contains("consensus task exited with error"),
        "wrapped consensus context missing: {err}"
    );
    assert!(
        err.contains("watchdog fatal"),
        "original consensus error missing: {err}"
    );
}

#[test]
fn consensus_thread_success_is_ok() {
    super::handle_consensus_thread_join(Ok(Ok(())))
        .expect("successful consensus thread must not error");
}

/// Full-node mode: dropping node_tx causes consensus thread's blocking_recv to return Err.
/// This verifies that the consensus thread exits immediately when no node handle is sent.
#[test]
fn test_fullnode_drops_node_tx_consensus_thread_exits() {
    let (node_tx, node_rx) = tokio::sync::oneshot::channel::<()>();

    // Simulate full-node path: drop sender without sending.
    drop(node_tx);

    // Consensus thread would call blocking_recv - should return Err immediately.
    let result = node_rx.blocking_recv();
    assert!(
        result.is_err(),
        "blocking_recv must return Err when sender is dropped"
    );
}
