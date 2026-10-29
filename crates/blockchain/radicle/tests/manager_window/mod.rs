use crate::{manager_support::*, wait_for};
use outbe_radicle::manager::{
    FinalizedBlock, FinalizedSnapshot, ManagerConfig, ManagerDependencies, ManagerError,
    ManagerPhase, PhaseError, RadicleManager, RadicleManagerHandle, RetryPolicy, SnapshotReader,
    MAX_REPOSITORIES_PER_RECONCILE,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc as std_mpsc, Arc, Mutex,
    },
    time::Duration,
};

fn start_long_repair(
    feed: Feed,
    snapshots: Snapshots,
    control: Control,
    availability: Availability,
) -> RadicleManagerHandle {
    RadicleManager::start(
        ManagerConfig {
            self_validator: SELF,
            local_node_id: LOCAL_NODE,
            repair_interval: Duration::from_secs(3_600),
            retry: RetryPolicy {
                base: Duration::from_secs(3_600),
                max: Duration::from_secs(3_600),
                jitter: Duration::ZERO,
            },
        },
        ManagerDependencies {
            finality: Arc::new(feed),
            snapshots: Arc::new(snapshots),
            endpoints: Arc::new(Endpoints::default()),
            control: Arc::new(control),
            repository_status: Arc::new(availability),
        },
    )
}

#[tokio::test]
async fn reconcile_window_is_capped() {
    let repos: Vec<_> = (0..1_000).map(repo_id).collect();
    let feed = Feed::new(block(10));
    let snapshots = Snapshots::default();
    snapshots.insert(snapshot(10, 1, repos.clone()));
    let control = Control::new(LOCAL_NODE);
    let availability = Availability::default();
    for &repo in &repos[..10] {
        availability.set(repo, true);
    }

    let handle = start_long_repair(feed, snapshots, control.clone(), availability.clone());
    let status = wait_for(&handle, 10).await;

    assert_eq!(
        control.seed_calls(),
        &repos[..MAX_REPOSITORIES_PER_RECONCILE]
    );
    assert_eq!(
        availability.calls(),
        &repos[..MAX_REPOSITORIES_PER_RECONCILE]
    );
    assert_eq!(status.desired_repository_count, 1_000);
    assert_eq!(
        status.observed_repository_count,
        MAX_REPOSITORIES_PER_RECONCILE
    );
    assert_eq!(status.available_repository_count, 10);
    assert_eq!(status.pending_repository_count, 6);
    assert_eq!(
        status.available_repository_count + status.pending_repository_count,
        status.observed_repository_count
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconcile_window_covers_every_repo() {
    let repos: Vec<_> = (0..1_000).map(repo_id).collect();
    let feed = Feed::new(block(1));
    let snapshots = Snapshots::default();
    for height in 1..=64 {
        snapshots.insert(snapshot(height, 1, repos.clone()));
    }
    let control = Control::new(LOCAL_NODE);
    let availability = Availability::default();

    let handle = start_long_repair(
        feed.clone(),
        snapshots,
        control.clone(),
        availability.clone(),
    );
    wait_for(&handle, 1).await;
    for height in 2..=63 {
        feed.push(block(height));
        wait_for(&handle, height).await;
    }

    let tail_status = handle.status();
    assert_eq!(tail_status.desired_repository_count, 1_000);
    assert_eq!(tail_status.observed_repository_count, 8);
    assert_eq!(control.seed_calls(), repos);
    assert_eq!(availability.calls(), repos);

    // Pass 64 starts the second sweep and must wrap cleanly to 0..16 rather than 8..24.
    control.clear_calls();
    availability.clear_calls();
    feed.push(block(64));
    let wrap_status = wait_for(&handle, 64).await;
    assert_eq!(
        wrap_status.observed_repository_count,
        MAX_REPOSITORIES_PER_RECONCILE
    );
    assert_eq!(
        control.seed_calls(),
        &repos[..MAX_REPOSITORIES_PER_RECONCILE]
    );
    assert_eq!(
        availability.calls(),
        &repos[..MAX_REPOSITORIES_PER_RECONCILE]
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconcile_window_survives_shrink() {
    let repos: Vec<_> = (0..1_000).map(repo_id).collect();
    let feed = Feed::new(block(1));
    let snapshots = Snapshots::default();
    snapshots.insert(snapshot(1, 1, repos.clone()));
    snapshots.insert(snapshot(2, 1, repos.clone()));
    snapshots.insert(snapshot(3, 1, repos[..3].to_vec()));
    snapshots.insert(snapshot(4, 1, vec![]));

    let mut mismatch = snapshot(5, 1, repos[..32].to_vec());
    mismatch.validators[0].node_id = Some([9; 32]);
    snapshots.insert(mismatch);
    snapshots.insert(snapshot(6, 1, repos[..32].to_vec()));
    snapshots.insert(snapshot(7, 1, repos[..32].to_vec()));
    snapshots.insert(snapshot(8, 1, repos[..32].to_vec()));

    let control = Control::new(LOCAL_NODE);
    let availability = Availability::default();
    let handle = start_long_repair(
        feed.clone(),
        snapshots,
        control.clone(),
        availability.clone(),
    );

    wait_for(&handle, 1).await;
    feed.push(block(2));
    wait_for(&handle, 2).await;

    // Shrink from 1,000 (cursor = 32) down to 3 repositories.
    control.clear_calls();
    availability.clear_calls();
    feed.push(block(3));
    let shrunk = wait_for(&handle, 3).await;
    assert_eq!(shrunk.desired_repository_count, 3);
    assert_eq!(shrunk.observed_repository_count, 3);
    assert_eq!(control.seed_calls(), &repos[..3]);
    assert_eq!(availability.calls(), &repos[..3]);

    // Shrink to 0 repositories (empty snapshot).
    control.clear_calls();
    availability.clear_calls();
    feed.push(block(4));
    let empty = wait_for(&handle, 4).await;
    assert_eq!(empty.desired_repository_count, 0);
    assert_eq!(empty.observed_repository_count, 0);
    assert_eq!(empty.available_repository_count, 0);
    assert_eq!(empty.pending_repository_count, 0);
    assert!(control.seed_calls().is_empty());
    assert!(availability.calls().is_empty());

    // Binding mismatch fails in binding_phase and must not advance the cursor from 0.
    feed.push(block(5));
    let mismatch_status = wait_for(&handle, 5).await;
    assert_eq!(
        mismatch_status.phase_error,
        Some(PhaseError::BindingMismatch)
    );
    assert!(control.seed_calls().is_empty());
    assert_eq!(availability.calls(), &repos[..16]);

    // First control.sessions() failure also aborts before seed_repositories and keeps cursor at 0.
    availability.clear_calls();
    control.fail_sessions(true);
    feed.push(block(6));
    let degraded = wait_for(&handle, 6).await;
    assert_eq!(degraded.phase, ManagerPhase::RuntimeDegraded);
    assert!(control.seed_calls().is_empty());
    assert_eq!(availability.calls(), &repos[..16]);

    // Entering seed_repositories commits next_cursor even if an individual repo's seed fails.
    availability.clear_calls();
    control.fail_sessions(false);
    control.fail_seed(repos[0], true);
    feed.push(block(7));
    wait_for(&handle, 7).await;
    assert_eq!(control.seed_calls(), &repos[..16]);
    assert_eq!(availability.calls(), &repos[..16]);

    control.clear_calls();
    availability.clear_calls();
    control.fail_seed(repos[0], false);
    feed.push(block(8));
    wait_for(&handle, 8).await;
    assert_eq!(control.seed_calls(), &repos[16..32]);
    assert_eq!(availability.calls(), &repos[16..32]);

    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconcile_window_is_identical_for_both_loops() {
    let repos: Vec<_> = (0..100).map(repo_id).collect();
    let feed = Feed::new(block(1));
    let snapshots = Snapshots::default();
    for height in 1..=3 {
        snapshots.insert(snapshot(height, 1, repos.clone()));
    }
    let control = Control::new(LOCAL_NODE);
    let availability = Availability::default();
    let handle = start_long_repair(
        feed.clone(),
        snapshots,
        control.clone(),
        availability.clone(),
    );

    for (height, expected) in [(1, &repos[0..16]), (2, &repos[16..32]), (3, &repos[32..48])] {
        if height > 1 {
            control.clear_calls();
            availability.clear_calls();
            feed.push(block(height));
        }
        wait_for(&handle, height).await;
        let seeded = control.seed_calls();
        let observed = availability.calls();
        assert_eq!(seeded, expected);
        assert_eq!(observed, seeded);
    }
    handle.shutdown().await.unwrap();
}

struct BlockingSnapshotReader {
    entered_tx: Mutex<Option<std_mpsc::Sender<()>>>,
    release_rx: Mutex<Option<std_mpsc::Receiver<()>>>,
    snapshot: FinalizedSnapshot,
}

impl SnapshotReader for BlockingSnapshotReader {
    fn read_exact(&self, _block: FinalizedBlock) -> Result<FinalizedSnapshot, ManagerError> {
        if let Some(entered_tx) = self.entered_tx.lock().unwrap().take() {
            let _ = entered_tx.send(());
        }
        if let Some(release_rx) = self.release_rx.lock().unwrap().take() {
            release_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("worker thread was blocked while read_exact waited");
        }
        Ok(self.snapshot.clone())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn load_pending_does_not_block_the_worker() {
    let (entered_tx, entered_rx) = std_mpsc::channel();
    let (release_tx, release_rx) = std_mpsc::channel();
    let snapshots = Arc::new(BlockingSnapshotReader {
        entered_tx: Mutex::new(Some(entered_tx)),
        release_rx: Mutex::new(Some(release_rx)),
        snapshot: snapshot(10, 1, vec![]),
    });
    let worker_ticks = Arc::new(AtomicUsize::new(0));
    let worker_ticks_clone = Arc::clone(&worker_ticks);

    let unblocker = tokio::spawn(async move {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            worker_ticks_clone.fetch_add(1, Ordering::Relaxed);
            if entered_rx.try_recv().is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for BlockingSnapshotReader::read_exact to start"
            );
            tokio::task::yield_now().await;
        }
        worker_ticks_clone.fetch_add(1, Ordering::Relaxed);
        release_tx.send(()).unwrap();
    });

    let handle = RadicleManager::start(
        ManagerConfig {
            self_validator: SELF,
            local_node_id: LOCAL_NODE,
            repair_interval: Duration::from_secs(3_600),
            retry: RetryPolicy::default(),
        },
        ManagerDependencies {
            finality: Arc::new(Feed::new(block(10))),
            snapshots,
            endpoints: Arc::new(Endpoints::default()),
            control: Arc::new(Control::new(LOCAL_NODE)),
            repository_status: Arc::new(Availability::default()),
        },
    );

    unblocker.await.unwrap();
    let status = wait_for(&handle, 10).await;
    assert_eq!(status.last_converged_finalized, Some(block(10)));
    assert!(worker_ticks.load(Ordering::Relaxed) >= 2);
    handle.shutdown().await.unwrap();
}

struct PanicOnceSnapshotReader {
    panicked: AtomicBool,
    snapshot: FinalizedSnapshot,
}

impl SnapshotReader for PanicOnceSnapshotReader {
    fn read_exact(&self, _block: FinalizedBlock) -> Result<FinalizedSnapshot, ManagerError> {
        if !self.panicked.swap(true, Ordering::SeqCst) {
            panic!("simulated snapshot reader panic");
        }
        Ok(self.snapshot.clone())
    }
}

#[tokio::test]
async fn load_pending_join_error_retries() {
    let snapshots = Arc::new(PanicOnceSnapshotReader {
        panicked: AtomicBool::new(false),
        snapshot: snapshot(10, 1, vec![]),
    });
    let handle = RadicleManager::start(
        ManagerConfig {
            self_validator: SELF,
            local_node_id: LOCAL_NODE,
            repair_interval: Duration::from_millis(20),
            retry: RetryPolicy {
                base: Duration::from_millis(10),
                max: Duration::from_millis(40),
                jitter: Duration::ZERO,
            },
        },
        ManagerDependencies {
            finality: Arc::new(Feed::new(block(10))),
            snapshots,
            endpoints: Arc::new(Endpoints::default()),
            control: Arc::new(Control::new(LOCAL_NODE)),
            repository_status: Arc::new(Availability::default()),
        },
    );

    let status = wait_for(&handle, 10).await;
    assert_eq!(status.provider_failures, 1);
    assert_eq!(status.last_seen_finalized, Some(block(10)));
    assert_eq!(status.last_converged_finalized, Some(block(10)));
    handle.shutdown().await.unwrap();
}
