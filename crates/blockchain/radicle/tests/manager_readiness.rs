mod manager_support;

use alloy_primitives::B256;
use manager_support::{block, snapshot, Availability, Control, Feed, Snapshots, LOCAL_NODE, SELF};
use outbe_radicle::{
    endpoint::{ChainIdentity, VerifiedEndpoint, HANDLE_DEADLINE},
    errors::EndpointLifecycleError,
    integration::{shutdown_bounded, EndpointNetwork, EndpointTaskOwner, RadicleStatusChannel},
    manager::{
        BoxFuture, EndpointResolver, FinalizedBlock, FinalizedSnapshot, ManagerConfig,
        ManagerDependencies, ManagerError, RadicleManager, RetryPolicy,
    },
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

/// Delegates refresh calls to the real resolver and signals entry before polling.
struct NotifyingResolver<R> {
    inner: R,
    entered: Mutex<Option<oneshot::Sender<tokio::time::Instant>>>,
    _drop_witness: oneshot::Sender<()>,
}

impl<R: EndpointResolver> EndpointResolver for NotifyingResolver<R> {
    fn refresh<'a>(
        &'a self,
        snapshot: &'a FinalizedSnapshot,
    ) -> BoxFuture<'a, Result<Vec<VerifiedEndpoint>, ManagerError>> {
        Box::pin(async move {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(tokio::time::Instant::now());
            }
            self.inner.refresh(snapshot).await
        })
    }
}

#[tokio::test(start_paused = true)]
async fn unstarted_endpoint_service_allows_bounded_manager_shutdown() {
    let (_, status) = RadicleStatusChannel::enabled(SELF, LOCAL_NODE);
    let (service, resolver, _) = EndpointNetwork::build(
        ChainIdentity {
            chain_id: 1,
            genesis_hash: B256::ZERO,
        },
        status,
    );
    let owner = EndpointTaskOwner::default();

    let feed = Feed::new(block(10));
    let snapshots = Snapshots::default();
    snapshots.insert(snapshot(10, 1, vec![]));

    let (entered_tx, entered_rx) = oneshot::channel();
    let (drop_witness_tx, mut drop_witness_rx) = oneshot::channel();

    // Keep the sole Arc inside ManagerDependencies so manager drop closes the witness.
    let manager = RadicleManager::start(
        ManagerConfig {
            self_validator: SELF,
            local_node_id: LOCAL_NODE,
            repair_interval: Duration::from_secs(30),
            retry: RetryPolicy {
                base: Duration::from_secs(1),
                max: Duration::from_secs(30),
                jitter: Duration::ZERO,
            },
        },
        ManagerDependencies {
            finality: Arc::new(feed),
            snapshots: Arc::new(snapshots),
            endpoints: Arc::new(NotifyingResolver {
                inner: resolver.clone(),
                entered: Mutex::new(Some(entered_tx)),
                _drop_witness: drop_witness_tx,
            }),
            control: Arc::new(Control::new(LOCAL_NODE)),
            repository_status: Arc::new(Availability::default()),
        },
    );

    let entry_instant = entered_rx
        .await
        .expect("manager must enter endpoint refresh");
    let shutdown_requested = tokio::time::Instant::now();
    assert_eq!(
        entry_instant, shutdown_requested,
        "shutdown request must occur at the same virtual instant as refresh entry"
    );

    let mut witness_closed_before_endpoint_cleanup = false;
    let mut endpoint_cleanup_result = None;
    let shutdown_result = shutdown_bounded(HANDLE_DEADLINE, manager, async {
        witness_closed_before_endpoint_cleanup = matches!(
            drop_witness_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        );
        assert!(
            witness_closed_before_endpoint_cleanup,
            "manager must drop its resolver witness before endpoint cleanup"
        );
        let result = owner.shutdown(&resolver).await;
        endpoint_cleanup_result = Some(result.clone());
        result
    })
    .await;
    let elapsed = shutdown_requested.elapsed();

    drop(service);

    assert!(
        witness_closed_before_endpoint_cleanup,
        "manager drop witness must close before endpoint cleanup"
    );
    assert_eq!(
        endpoint_cleanup_result,
        Some(Ok(())),
        "unstarted endpoint owner cleanup must succeed"
    );
    assert_eq!(
        shutdown_result,
        Ok(()),
        "manager shutdown must succeed before endpoint cleanup (elapsed: {elapsed:?})"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "manager shutdown must finish strictly before the 4s graceful drain, elapsed: {elapsed:?}"
    );
}

/// Delegates refresh calls to the real resolver and reports each completed result.
struct ReportingResolver<R> {
    inner: R,
    results: mpsc::UnboundedSender<(FinalizedBlock, Result<Vec<VerifiedEndpoint>, ManagerError>)>,
}

impl<R: EndpointResolver> EndpointResolver for ReportingResolver<R> {
    fn refresh<'a>(
        &'a self,
        snapshot: &'a FinalizedSnapshot,
    ) -> BoxFuture<'a, Result<Vec<VerifiedEndpoint>, ManagerError>> {
        let block = snapshot.block;
        Box::pin(async move {
            let result = self.inner.refresh(snapshot).await;
            let _ = self.results.send((block, result.clone()));
            result
        })
    }
}

#[tokio::test(start_paused = true)]
async fn manager_processes_finalized_blocks_without_refresh_stalls_while_unstarted() {
    let (_, status) = RadicleStatusChannel::enabled(SELF, LOCAL_NODE);
    let (service, resolver, _) = EndpointNetwork::build(
        ChainIdentity {
            chain_id: 1,
            genesis_hash: B256::ZERO,
        },
        status,
    );
    let owner = EndpointTaskOwner::default();

    let feed = Feed::new(block(10));
    let snapshots = Snapshots::default();
    for height in 10..=13 {
        snapshots.insert(snapshot(height, 1, vec![]));
    }

    let (results_tx, mut results_rx) = mpsc::unbounded_channel();
    let started = tokio::time::Instant::now();
    let manager = RadicleManager::start(
        ManagerConfig {
            self_validator: SELF,
            local_node_id: LOCAL_NODE,
            repair_interval: Duration::from_secs(30),
            retry: RetryPolicy {
                base: Duration::from_secs(10),
                max: Duration::from_secs(30),
                jitter: Duration::ZERO,
            },
        },
        ManagerDependencies {
            finality: Arc::new(feed.clone()),
            snapshots: Arc::new(snapshots.clone()),
            endpoints: Arc::new(ReportingResolver {
                inner: resolver.clone(),
                results: results_tx,
            }),
            control: Arc::new(Control::new(LOCAL_NODE)),
            repository_status: Arc::new(Availability::default()),
        },
    );

    let expected_error = ManagerError::from(EndpointLifecycleError::NotStarted);
    assert_ne!(expected_error, ManagerError::Stopped);

    // Wait for the sampled block 10 refresh before pushing blocks 11 through 13.
    let (seen_block, seen_result) = results_rx
        .recv()
        .await
        .expect("manager must refresh initial sampled block");
    assert_eq!(seen_block, block(10));
    assert_eq!(seen_result, Err(expected_error.clone()));

    for height in 11..=13 {
        feed.push(block(height));
        let (seen_block, seen_result) = results_rx
            .recv()
            .await
            .expect("manager must refresh each finalized block");
        assert_eq!(seen_block, block(height));
        assert_eq!(seen_result, Err(expected_error.clone()));
    }

    // Wait for the manager to publish status for the final consumed block.
    loop {
        let status = manager.status();
        if status.last_seen_finalized == Some(block(13)) && status.endpoint_failures >= 4 {
            break;
        }
        tokio::task::yield_now().await;
    }

    let elapsed = started.elapsed();
    let status = manager.status();
    assert!(
        elapsed < HANDLE_DEADLINE,
        "manager must process finalized blocks without 5s refresh stalls, elapsed: {elapsed:?}"
    );
    assert_eq!(status.last_seen_finalized, Some(block(13)));
    assert_eq!(status.endpoint_failures, 4);
    assert_eq!(
        snapshots.reads(),
        vec![block(10), block(11), block(12), block(13)]
    );

    shutdown_bounded(HANDLE_DEADLINE, manager, owner.shutdown(&resolver))
        .await
        .expect("manager and unstarted endpoint owner must shut down cleanly");
    drop(service);
}
