//! Reth ExEx adapter for finalized offchain-data projection.
//!
//! Canonical-chain notifications are deliberately only drained here. The provider's finalized
//! block signal is the sole authority that permits projection writes.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use alloy_consensus::{transaction::TxHashRef, BlockHeader, TxReceipt};
use alloy_primitives::{Sealable, B256};
use eyre::{bail, Context};
use futures::{FutureExt, Stream, StreamExt};
use metrics::{counter, gauge};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
    ProjectionFailure, ProjectionFailureClass, ProjectionOutcome, ProjectionReadinessHandle,
    ProjectionReadinessPublisher, ProjectionStatus, RuntimeBodyFailure, RuntimeBodyReaders,
};
use outbe_offchain_storage::{
    MongoStorage, MongoStorageConfig, MongoWriterLease, StorageError, StorageErrorKind,
    StorageReaderHandle, StorageWriterHandle,
};
use outbe_primitives::{
    chain::{is_devnet, is_testnet},
    projection::{projection_readiness, ProjectionCheckpoint},
};
use reth_chain_state::ForkChoiceSubscriptions;
use reth_ethereum::exex::{ExExContext, ExExEvent};
use reth_node_builder::FullNodeComponents;
use reth_primitives_traits::{Block, BlockBody};
use reth_provider::{BlockHashReader, BlockIdReader, BlockReader};
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};

const PROJECTION_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const MONGO_RECONNECT_DEADLINE: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalizedTarget {
    number: u64,
    hash: B256,
}

impl FinalizedTarget {
    const fn new(number: u64, hash: B256) -> Self {
        Self { number, hash }
    }
}

#[derive(Debug, thiserror::Error)]
enum HistoricalProjectionDataError {
    #[error("canonical block {block_number} is unavailable")]
    CanonicalBlock { block_number: u64 },
    #[error("canonical block {block_number} ({block_hash}) is unavailable by hash")]
    CanonicalBlockByHash { block_number: u64, block_hash: B256 },
    #[error("receipts for canonical block {block_number} are unavailable")]
    Receipts { block_number: u64 },
}

type ProjectionAttempt = tokio::sync::oneshot::Receiver<eyre::Result<Option<FinalizedTarget>>>;

fn spawn_detached_projection_work<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<tokio::sync::oneshot::Receiver<T>> {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _ = result_tx.send(work());
        })?;
    Ok(result_rx)
}

/// Structured terminal condition reported to the top-level node lifecycle owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionExit {
    pub failure: ProjectionFailure,
}

/// Complete startup configuration for the required finalized offchain-data projection.
#[derive(Clone)]
pub struct OffchainDataProjectionConfig {
    /// EVM chain identity recorded in the managed projection state.
    pub chain_id: u64,
    /// Canonical genesis hash recorded in the managed projection state.
    pub genesis_hash: B256,
    /// First finalized block eligible for projection.
    pub start_block: u64,
    /// MongoDB connection string.
    pub mongodb_uri: String,
    /// MongoDB database containing projection namespaces.
    pub mongodb_database: String,
}

/// Projection instance whose MongoDB connection, topology, and managed state passed preflight.
pub struct PreparedOffchainDataProjection {
    projector: OffchainDataProjection,
    storage: Arc<MongoStorage>,
    writer_lease: MongoWriterLease,
    readiness_publisher: ProjectionReadinessPublisher,
    readiness: ProjectionReadinessHandle,
    runtime_failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
    runtime_failure_receiver: tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>,
}

impl PreparedOffchainDataProjection {
    /// Typed read-only capabilities injected into EVM execution.
    #[must_use]
    pub fn runtime_body_readers(&self) -> RuntimeBodyReaders {
        let reader: StorageReaderHandle = self.storage.clone();
        RuntimeBodyReaders::new_supervised(reader, self.runtime_failure_sender.clone())
    }

    /// Backend-neutral exact-checkpoint readiness used by local execution gates.
    #[must_use]
    pub fn readiness(&self) -> ProjectionReadinessHandle {
        self.readiness.clone()
    }
}

/// Projection instance whose available canonical checkpoint identity passed startup checks.
pub struct ReadyOffchainDataProjection {
    projector: OffchainDataProjection,
    readiness_publisher: ProjectionReadinessPublisher,
    projection_config: ProjectionConfig,
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
    writer_lease: MongoWriterLease,
    runtime_failure_receiver: tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>,
}

/// Connects to MongoDB and validates storage prerequisites before Reth component initialization.
pub fn prepare_offchain_data_projection(
    config: OffchainDataProjectionConfig,
) -> eyre::Result<PreparedOffchainDataProjection> {
    if !is_devnet(config.chain_id) && !is_testnet(config.chain_id) {
        bail!(
            "ADR-005 Mongo execution reads are disabled outside Outbe devnet/testnet (chain_id {})",
            config.chain_id
        );
    }
    if config.start_block != 1 {
        bail!(
            "ADR-005 requires projection start_block 1, found {}",
            config.start_block
        );
    }

    let started = std::time::Instant::now();
    loop {
        let remaining = MONGO_RECONNECT_DEADLINE.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            bail!("MongoDB startup recovery exceeded the eight-second total deadline");
        }
        let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(1);
        let attempt_config = config.clone();
        std::thread::Builder::new()
            .name("offchain-startup".to_owned())
            .spawn(move || {
                let _ = attempt_tx.send(prepare_projection_attempt(&attempt_config));
            })
            .wrap_err("spawn MongoDB startup validation worker")?;
        let attempt = match attempt_rx.recv_timeout(remaining) {
            Ok(attempt) => attempt,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                bail!("MongoDB startup recovery exceeded the eight-second total deadline");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("MongoDB startup validation worker exited unexpectedly");
            }
        };
        match attempt {
            Ok((storage, projector, writer_lease)) => {
                let initial = match projector.state().checkpoint {
                    Some(checkpoint) => ProjectionStatus::CatchingUp {
                        checkpoint: Some(checkpoint),
                    },
                    None => ProjectionStatus::Starting,
                };
                let (readiness_publisher, readiness) = projection_readiness(
                    ProjectionCheckpoint {
                        block_number: 0,
                        block_hash: config.genesis_hash,
                    },
                    initial,
                );
                let (runtime_failure_sender, runtime_failure_receiver) =
                    tokio::sync::watch::channel(None);
                return Ok(PreparedOffchainDataProjection {
                    projector,
                    storage,
                    writer_lease,
                    readiness_publisher,
                    readiness,
                    runtime_failure_sender,
                    runtime_failure_receiver,
                });
            }
            Err(error)
                if error.is_unavailable() && started.elapsed() < MONGO_RECONNECT_DEADLINE =>
            {
                let remaining = MONGO_RECONNECT_DEADLINE.saturating_sub(started.elapsed());
                std::thread::sleep(PROJECTION_RETRY_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error.into_eyre()),
        }
    }
}

enum PrepareProjectionError {
    Storage(StorageError),
    Projection(outbe_offchain_data::ProjectionError),
}

impl PrepareProjectionError {
    fn is_unavailable(&self) -> bool {
        match self {
            Self::Storage(error)
            | Self::Projection(outbe_offchain_data::ProjectionError::Storage(error)) => {
                error.kind() == StorageErrorKind::Unavailable
            }
            Self::Projection(_) => false,
        }
    }

    fn into_eyre(self) -> eyre::Report {
        match self {
            Self::Storage(error) => eyre::Report::new(error),
            Self::Projection(error) => eyre::Report::new(error),
        }
    }
}

fn prepare_projection_attempt(
    config: &OffchainDataProjectionConfig,
) -> Result<(Arc<MongoStorage>, OffchainDataProjection, MongoWriterLease), PrepareProjectionError> {
    let projection_config = ProjectionConfig {
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        start_block: config.start_block,
    };
    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri: config.mongodb_uri.clone(),
            database: config.mongodb_database.clone(),
        })
        .map_err(PrepareProjectionError::Storage)?,
    );
    storage
        .verify_transaction_support()
        .map_err(PrepareProjectionError::Storage)?;
    let writer_lease = storage
        .acquire_writer_lease()
        .map_err(PrepareProjectionError::Storage)?;
    gauge!("outbe_projection_mongo_topology_capable").set(1.0);
    let reader: StorageReaderHandle = storage.clone();
    let projector = OffchainDataProjection::open(projection_config, reader, storage.clone())
        .map_err(PrepareProjectionError::Projection)?;
    storage
        .verify_acknowledged_transaction()
        .map_err(PrepareProjectionError::Storage)?;
    Ok((storage, projector, writer_lease))
}

/// Validates a persisted checkpoint against canonical Reth state during ExEx initialization.
pub fn validate_offchain_data_checkpoint<P>(
    prepared: PreparedOffchainDataProjection,
    canonical_hashes: &P,
) -> eyre::Result<ReadyOffchainDataProjection>
where
    P: BlockHashReader + BlockIdReader,
{
    let projector = prepared.projector;
    let projection_config = ProjectionConfig {
        chain_id: projector.state().chain_id,
        genesis_hash: projector.state().genesis_hash,
        start_block: projector.state().start_block,
    };
    let reader: StorageReaderHandle = prepared.storage.clone();
    let writer: StorageWriterHandle = prepared.storage;
    let readiness_publisher = prepared.readiness_publisher;
    let runtime_failure_receiver = prepared.runtime_failure_receiver;
    let writer_lease = prepared.writer_lease;
    let local_finalized = canonical_hashes
        .finalized_block_num_hash()
        .wrap_err("read local Reth finalized checkpoint for offchain-data validation")?
        .map(|block| FinalizedTarget::new(block.number, block.hash));
    if let Some(checkpoint) = projector.state().checkpoint {
        let local_finalized = require_finalized_checkpoint(checkpoint, local_finalized)?;
        match canonical_hashes
            .block_hash(checkpoint.block_number)
            .wrap_err("read canonical Reth hash for offchain-data checkpoint validation")?
        {
            Some(canonical_hash) if canonical_hash == checkpoint.block_hash => {}
            Some(canonical_hash) => bail!(
                "offchain-data MongoDB checkpoint identity mismatch at block {}: stored {}, canonical {}",
                checkpoint.block_number,
                checkpoint.block_hash,
                canonical_hash
            ),
            None => bail!(
                "canonical block {} for Mongo checkpoint {} is unavailable locally",
                checkpoint.block_number,
                checkpoint.block_hash
            ),
        }
        if checkpoint.block_number == local_finalized.number {
            publish_status(
                &readiness_publisher,
                ProjectionStatus::Ready { checkpoint },
                Some(FinalizedTarget::new(
                    local_finalized.number,
                    local_finalized.hash,
                )),
            );
        } else {
            publish_status(
                &readiness_publisher,
                ProjectionStatus::CatchingUp {
                    checkpoint: Some(checkpoint),
                },
                Some(FinalizedTarget::new(
                    local_finalized.number,
                    local_finalized.hash,
                )),
            );
        }
    } else {
        let target = local_finalized.map(|block| FinalizedTarget::new(block.number, block.hash));
        let status = match target {
            Some(target) if target.number == 0 && target.hash == projection_config.genesis_hash => {
                ProjectionStatus::Ready {
                    checkpoint: ProjectionCheckpoint {
                        block_number: 0,
                        block_hash: projection_config.genesis_hash,
                    },
                }
            }
            _ => ProjectionStatus::CatchingUp { checkpoint: None },
        };
        publish_status(&readiness_publisher, status, target);
    }
    let projection_state = projector.state();
    info!(
        chain_id = projection_state.chain_id,
        genesis_hash = %projection_state.genesis_hash,
        start_block = projection_state.start_block,
        "finalized offchain-data projection ready"
    );
    Ok(ReadyOffchainDataProjection {
        projector,
        readiness_publisher,
        projection_config,
        reader,
        writer,
        writer_lease,
        runtime_failure_receiver,
    })
}

fn require_finalized_checkpoint(
    checkpoint: ProjectionCheckpoint,
    local_finalized: Option<FinalizedTarget>,
) -> eyre::Result<FinalizedTarget> {
    let Some(local_finalized) = local_finalized else {
        bail!(
            "projection_ahead_of_execution: Mongo checkpoint {} ({}) exists before local Reth finality",
            checkpoint.block_number,
            checkpoint.block_hash
        );
    };
    if checkpoint.block_number > local_finalized.number.saturating_add(1) {
        bail!(
            "projection_ahead_of_execution: Mongo checkpoint {} ({}) is ahead of local Reth finalized {} ({})",
            checkpoint.block_number,
            checkpoint.block_hash,
            local_finalized.number,
            local_finalized.hash
        );
    }
    if checkpoint.block_number == local_finalized.number
        && checkpoint.block_hash != local_finalized.hash
    {
        bail!(
            "offchain-data MongoDB checkpoint {} ({}) does not match local Reth finalized {} ({})",
            checkpoint.block_number,
            checkpoint.block_hash,
            local_finalized.number,
            local_finalized.hash
        );
    }
    Ok(local_finalized)
}

struct ProjectionRuntime {
    projector: OffchainDataProjection,
    readiness_publisher: ProjectionReadinessPublisher,
    projection_config: ProjectionConfig,
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
    writer_lease: Option<MongoWriterLease>,
    runtime_failure_receiver: Option<tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>>,
}

impl ProjectionRuntime {
    fn new(ready: ReadyOffchainDataProjection) -> Self {
        Self {
            projector: ready.projector,
            readiness_publisher: ready.readiness_publisher,
            projection_config: ready.projection_config,
            reader: ready.reader,
            writer: ready.writer,
            writer_lease: Some(ready.writer_lease),
            runtime_failure_receiver: Some(ready.runtime_failure_receiver),
        }
    }

    fn writer_lease_is_valid(&self) -> bool {
        self.writer_lease
            .as_ref()
            .is_none_or(MongoWriterLease::is_valid)
    }
}

/// Runs finalized offchain-data projection as a Reth execution extension.
///
/// Projection and provider reads run on a blocking worker because the configured storage backend
/// is synchronous. The async task remains available to drain ordinary ExEx notifications while
/// that work is in flight.
pub async fn run_offchain_data_projection<Node>(
    ctx: ExExContext<Node>,
    ready: ReadyOffchainDataProjection,
    projection_exit: tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
{
    let provider = ctx.provider().clone();
    let finalized_blocks = provider.finalized_block_stream().map(|header| {
        let block = header.num_hash();
        FinalizedTarget::new(block.number, block.hash)
    });
    let notifications = ctx
        .notifications
        .map(|notification| notification.map(|_| ()).map_err(|error| error.to_string()));
    run_projection_loop(
        provider,
        notifications,
        finalized_blocks,
        ctx.events,
        ProjectionRuntime::new(ready),
        projection_exit,
    )
    .await
}

/// Keeps the installed ExEx future alive and converts an unexpected return or panic into the
/// projection supervisor's structured shutdown path.
pub async fn supervise_offchain_data_projection<Node>(
    ctx: ExExContext<Node>,
    ready: ReadyOffchainDataProjection,
    projection_exit: tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
{
    let publisher = ready.readiness_publisher.clone();
    supervise_projection_future(
        run_offchain_data_projection(ctx, ready, projection_exit.clone()),
        publisher,
        projection_exit,
    )
    .await
}

async fn supervise_projection_future<F>(
    future: F,
    publisher: ProjectionReadinessPublisher,
    projection_exit: tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
) -> eyre::Result<()>
where
    F: std::future::Future<Output = eyre::Result<()>>,
{
    let message = match std::panic::AssertUnwindSafe(future).catch_unwind().await {
        Ok(Ok(())) => "offchain-data ExEx returned unexpectedly".to_owned(),
        Ok(Err(error)) => format!("offchain-data ExEx failed: {error}"),
        Err(_) => "offchain-data ExEx panicked".to_owned(),
    };
    publish_fatal(
        &publisher,
        &projection_exit,
        ProjectionFailureClass::ProjectorExited,
        message,
    );
    std::future::pending().await
}

async fn run_projection_loop<P, N, F>(
    provider: P,
    mut notifications: N,
    mut finalized_blocks: F,
    events: tokio::sync::mpsc::UnboundedSender<ExExEvent>,
    runtime: ProjectionRuntime,
    projection_exit: tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
) -> eyre::Result<()>
where
    P: BlockIdReader + BlockReader + Clone + Send + 'static,
    N: Stream<Item = Result<(), String>> + Unpin,
    F: Stream<Item = FinalizedTarget> + Unpin,
{
    let mut runtime = runtime;
    let start_block = runtime.projector.state().start_block;
    let durable_startup_checkpoint = runtime
        .projector
        .state()
        .checkpoint
        .map(|checkpoint| FinalizedTarget::new(checkpoint.block_number, checkpoint.block_hash));
    let recovery_baseline = FinalizedTarget::new(0, runtime.projection_config.genesis_hash);
    let readiness_publisher = runtime.readiness_publisher.clone();
    let mut runtime_failures = runtime
        .runtime_failure_receiver
        .take()
        .ok_or_else(|| eyre::eyre!("projection body-read failure receiver is unavailable"))?;
    let projector = Arc::new(Mutex::new(runtime));
    let (durable_checkpoint_tx, mut durable_checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recovery_ack_tx, mut recovery_ack_rx) = tokio::sync::mpsc::unbounded_channel();

    // `finalized_block_stream` emits only changes, so the current provider value must be sampled
    // separately to avoid waiting forever when the node starts at an already-finalized height.
    let initial_target = match provider.finalized_block_num_hash() {
        Ok(block) => block.map(|block| FinalizedTarget::new(block.number, block.hash)),
        Err(error) => {
            warn!(%error, "failed to sample current finalized block; retrying later");
            None
        }
    };

    let mut startup_checkpoint_floor = match (durable_startup_checkpoint, initial_target) {
        (Some(checkpoint), Some(target)) if target.number < checkpoint.number => Some(checkpoint),
        _ => None,
    };
    let initial_target = initial_target.filter(|_| startup_checkpoint_floor.is_none());
    let mut latest_target = initial_target;
    let mut pending_target = initial_target;
    let mut projection_attempt: Option<ProjectionAttempt> = None;
    let mut can_start_attempt = true;
    let mut finality_stalled = false;
    let mut notifications_open = true;
    let mut finalized_stream_open = true;
    let mut runtime_failures_open = true;
    let mut mongo_unavailable_since: Option<tokio::time::Instant> = None;
    let mut immediate_recovery_used = false;

    let retry_start = tokio::time::Instant::now() + PROJECTION_RETRY_INTERVAL;
    let mut retry = tokio::time::interval_at(retry_start, PROJECTION_RETRY_INTERVAL);
    retry.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        if projection_attempt.is_none() && !finality_stalled {
            let lease_valid = projector
                .lock()
                .map(|runtime| runtime.writer_lease_is_valid())
                .unwrap_or(false);
            if !lease_valid {
                publish_fatal(
                    &readiness_publisher,
                    &projection_exit,
                    ProjectionFailureClass::WriterLeaseLost,
                    "MongoDB projection writer lease was lost",
                );
                can_start_attempt = false;
                finality_stalled = true;
            }
        }
        if projection_attempt.is_none() && can_start_attempt && !finality_stalled {
            if let Some(target) = pending_target {
                let provider = provider.clone();
                let projector = Arc::clone(&projector);
                let durable_checkpoint_tx = durable_checkpoint_tx.clone();
                let recovery_ack_tx = recovery_ack_tx.clone();
                match spawn_detached_projection_work("offchain-projector", move || {
                    project_through_target(
                        provider,
                        &projector,
                        target,
                        &durable_checkpoint_tx,
                        &recovery_ack_tx,
                    )
                }) {
                    Ok(result_rx) => {
                        projection_attempt = Some(result_rx);
                        can_start_attempt = false;
                    }
                    Err(error) => {
                        publish_fatal(
                            &readiness_publisher,
                            &projection_exit,
                            ProjectionFailureClass::ProjectorExited,
                            format!("failed to spawn offchain-data projection worker: {error}"),
                        );
                        can_start_attempt = false;
                        finality_stalled = true;
                    }
                }
            }
        }

        tokio::select! {
            notification = notifications.next(), if notifications_open => {
                match notification {
                    Some(Ok(())) => {
                        // Receiving the notification is the required action. Canonical commit and
                        // reorg notifications never authorize off-chain writes.
                    }
                    None => {
                        notifications_open = false;
                        warn!("offchain-data ExEx notification stream closed");
                    }
                    Some(Err(error)) => {
                        // A malformed/backfill notification must not kill the ExEx. Continue
                        // polling so the manager is not backpressured by this projection.
                        warn!(%error, "failed to drain offchain-data ExEx notification");
                    }
                }
            }

            finalized = finalized_blocks.next(), if finalized_stream_open => {
                match finalized {
                    Some(target) => {
                        match admit_startup_finalized_target(&mut startup_checkpoint_floor, target) {
                            Ok(false) => continue,
                            Ok(true) => {}
                            Err(error) => {
                                publish_fatal(
                                    &readiness_publisher,
                                    &projection_exit,
                                    ProjectionFailureClass::CheckpointMismatch,
                                    error.to_string(),
                                );
                                can_start_attempt = false;
                                finality_stalled = true;
                                continue;
                            }
                        }
                        if record_or_publish_finalized_target(
                            &mut latest_target,
                            &mut pending_target,
                            target,
                            &readiness_publisher,
                            &projection_exit,
                        ) {
                            can_start_attempt = true;
                        } else {
                            can_start_attempt = false;
                            finality_stalled = true;
                        }
                    }
                    None => {
                        finalized_stream_open = false;
                        warn!("offchain-data finalized block stream closed");
                    }
                }
            }

            changed = runtime_failures.changed(), if runtime_failures_open && !finality_stalled => {
                match changed {
                    Ok(()) => match runtime_failures.borrow_and_update().clone() {
                    Some(RuntimeBodyFailure::Unavailable) => {
                        let since = *mongo_unavailable_since
                            .get_or_insert_with(tokio::time::Instant::now);
                        publish_status(
                            &readiness_publisher,
                            ProjectionStatus::MongoUnavailable {
                                checkpoint: readiness_checkpoint(&readiness_publisher.current()),
                                since: since.into_std(),
                            },
                            latest_target,
                        );
                        gauge!("outbe_projection_mongo_reconnect_active").set(1.0);
                        gauge!("outbe_projection_mongo_reconnect_remaining_seconds")
                            .set(MONGO_RECONNECT_DEADLINE.as_secs_f64());
                        let recovery_target = latest_target.unwrap_or(recovery_baseline);
                        pending_target = Some(match pending_target {
                            Some(pending) if pending.number > recovery_target.number => pending,
                            _ => recovery_target,
                        });
                        can_start_attempt = projection_attempt.is_none();
                        immediate_recovery_used = true;
                    }
                    Some(RuntimeBodyFailure::Fatal(failure)) => {
                        publish_projection_failure(
                            &readiness_publisher,
                            &projection_exit,
                            failure,
                        );
                        finality_stalled = true;
                        can_start_attempt = false;
                    }
                    None => {}
                    },
                    Err(_) => runtime_failures_open = false,
                }
            }

            result = async {
                match projection_attempt.as_mut() {
                    Some(attempt) => attempt.await,
                    None => std::future::pending().await,
                }
            }, if projection_attempt.is_some() => {
                projection_attempt = None;
                if finality_stalled {
                    continue;
                }
                match result {
                    Ok(Ok(durable_checkpoint)) => {
                        let attempted_target = pending_target;
                        mongo_unavailable_since = None;
                        immediate_recovery_used = false;
                        if pending_target.is_some_and(|pending| {
                            durable_checkpoint.map_or(
                                pending.number < start_block,
                                |checkpoint| pending.number <= checkpoint.number,
                            )
                        }) {
                            pending_target = None;
                        }
                        can_start_attempt = pending_target.is_some();
                        publish_progress(
                            &readiness_publisher,
                            durable_checkpoint
                                .map(|checkpoint| ProjectionCheckpoint {
                                    block_number: checkpoint.number,
                                    block_hash: checkpoint.hash,
                                })
                                .or_else(|| {
                                    attempted_target
                                        .filter(|target| target.number < start_block)
                                        .map(|_| ProjectionCheckpoint {
                                            block_number: recovery_baseline.number,
                                            block_hash: recovery_baseline.hash,
                                        })
                                }),
                            pending_target,
                        );
                    }
                    Ok(Err(error)) => {
                        if projection_is_unavailable(&error) {
                            let since = *mongo_unavailable_since
                                .get_or_insert_with(tokio::time::Instant::now);
                            publish_status(
                                &readiness_publisher,
                                ProjectionStatus::MongoUnavailable {
                                    checkpoint: readiness_checkpoint(&readiness_publisher.current()),
                                    since: since.into_std(),
                                },
                                latest_target,
                            );
                            gauge!("outbe_projection_mongo_reconnect_active").set(1.0);
                            gauge!("outbe_projection_mongo_reconnect_remaining_seconds").set(
                                MONGO_RECONNECT_DEADLINE
                                    .saturating_sub(since.elapsed())
                                    .as_secs_f64(),
                            );
                            if since.elapsed() >= MONGO_RECONNECT_DEADLINE {
                                publish_fatal(
                                    &readiness_publisher,
                                    &projection_exit,
                                    ProjectionFailureClass::MongoReconnectDeadline,
                                    "MongoDB reconnect deadline expired",
                                );
                                finality_stalled = true;
                                can_start_attempt = false;
                            } else if !immediate_recovery_used {
                                immediate_recovery_used = true;
                                can_start_attempt = true;
                            } else {
                                can_start_attempt = false;
                            }
                            warn!("finalized offchain-data projection unavailable; recovery active");
                        } else {
                            error!(%error, "fatal finalized offchain-data projection failure");
                            publish_fatal(
                                &readiness_publisher,
                                &projection_exit,
                                projection_failure_class(&error),
                                error.to_string(),
                            );
                            finality_stalled = true;
                            can_start_attempt = false;
                        }
                    }
                    Err(error) => {
                        error!(%error, "finalized offchain-data projection worker failed");
                        publish_fatal(
                            &readiness_publisher,
                            &projection_exit,
                            ProjectionFailureClass::ProjectorExited,
                            "offchain-data projection worker exited unexpectedly",
                        );
                        finality_stalled = true;
                        can_start_attempt = false;
                    }
                }
            }

            checkpoint = durable_checkpoint_rx.recv(), if !finality_stalled => {
                if let Some(checkpoint) = checkpoint {
                    let projection_checkpoint = ProjectionCheckpoint {
                        block_number: checkpoint.number,
                        block_hash: checkpoint.hash,
                    };
                    let caught_up = latest_target.is_some_and(|target| target == checkpoint);
                    publish_status(&readiness_publisher, if caught_up {
                        ProjectionStatus::Ready {
                            checkpoint: projection_checkpoint,
                        }
                    } else {
                        ProjectionStatus::CatchingUp {
                            checkpoint: Some(projection_checkpoint),
                        }
                    }, latest_target);
                    let finished = (checkpoint.number, checkpoint.hash).into();
                    if events.send(ExExEvent::FinishedHeight(finished)).is_err() {
                        // The manager channel can disappear during shutdown. Returning from a
                        // critical ExEx task would turn that into a node panic, so remain alive.
                        warn!("failed to publish durable offchain-data height");
                    } else {
                        info!(
                            block_number = checkpoint.number,
                            block_hash = %checkpoint.hash,
                            "finalized offchain-data projection checkpoint advanced"
                        );
                    }
                }
            }

            recovered = recovery_ack_rx.recv(), if !finality_stalled => {
                if recovered.is_some() && mongo_unavailable_since.take().is_some() {
                    immediate_recovery_used = false;
                    publish_status(
                        &readiness_publisher,
                        ProjectionStatus::CatchingUp {
                            checkpoint: readiness_checkpoint(&readiness_publisher.current()),
                        },
                        latest_target,
                    );
                    gauge!("outbe_projection_mongo_reconnect_active").set(0.0);
                    gauge!("outbe_projection_mongo_reconnect_remaining_seconds").set(0.0);
                }
            }

            _ = async {
                match mongo_unavailable_since {
                    Some(since) => tokio::time::sleep_until(since + MONGO_RECONNECT_DEADLINE).await,
                    None => std::future::pending().await,
                }
            }, if !finality_stalled => {
                publish_fatal(
                    &readiness_publisher,
                    &projection_exit,
                    ProjectionFailureClass::MongoReconnectDeadline,
                    "MongoDB reconnect deadline expired",
                );
                finality_stalled = true;
                can_start_attempt = false;
            }

            _ = retry.tick(), if projection_attempt.is_none() && !finality_stalled => {
                if pending_target.is_some() {
                    can_start_attempt = true;
                } else {
                    match provider.finalized_block_num_hash() {
                        Ok(Some(block)) => {
                            let target = FinalizedTarget::new(block.number, block.hash);
                            match admit_startup_finalized_target(
                                &mut startup_checkpoint_floor,
                                target,
                            ) {
                                Ok(false) => continue,
                                Ok(true) => {}
                                Err(error) => {
                                    publish_fatal(
                                        &readiness_publisher,
                                        &projection_exit,
                                        ProjectionFailureClass::CheckpointMismatch,
                                        error.to_string(),
                                    );
                                    can_start_attempt = false;
                                    finality_stalled = true;
                                    continue;
                                }
                            }
                            if record_or_publish_finalized_target(
                                &mut latest_target,
                                &mut pending_target,
                                target,
                                &readiness_publisher,
                                &projection_exit,
                            ) {
                                can_start_attempt = true;
                            } else {
                                can_start_attempt = false;
                                finality_stalled = true;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!(%error, "failed to sample current finalized block; retrying later");
                        }
                    }
                }
            }

            // Keep the critical ExEx task alive even if its input channels have closed. A normal
            // return from an installed ExEx is treated as a critical task failure by Reth.
            () = std::future::pending::<()>() => {}
        }
    }
}

fn readiness_checkpoint(status: &ProjectionStatus) -> Option<ProjectionCheckpoint> {
    match status {
        ProjectionStatus::CatchingUp { checkpoint }
        | ProjectionStatus::MongoUnavailable { checkpoint, .. } => *checkpoint,
        ProjectionStatus::Ready { checkpoint } => Some(*checkpoint),
        ProjectionStatus::Fatal { checkpoint, .. } => *checkpoint,
        ProjectionStatus::Starting => None,
    }
}

fn publish_progress(
    publisher: &ProjectionReadinessPublisher,
    checkpoint: Option<ProjectionCheckpoint>,
    pending: Option<FinalizedTarget>,
) {
    let caught_up = match (checkpoint, pending) {
        (Some(checkpoint), Some(pending)) => {
            checkpoint.block_number == pending.number && checkpoint.block_hash == pending.hash
        }
        (Some(_), None) => true,
        (None, _) => false,
    };
    publish_status(
        publisher,
        match (caught_up, checkpoint) {
            (true, Some(checkpoint)) => ProjectionStatus::Ready { checkpoint },
            (_, checkpoint) => ProjectionStatus::CatchingUp { checkpoint },
        },
        pending,
    );
}

fn publish_fatal(
    publisher: &ProjectionReadinessPublisher,
    exit: &tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
    class: ProjectionFailureClass,
    message: impl Into<Arc<str>>,
) {
    let failure = ProjectionFailure::new(class, message);
    publish_projection_failure(publisher, exit, failure);
}

fn publish_projection_failure(
    publisher: &ProjectionReadinessPublisher,
    exit: &tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
    failure: ProjectionFailure,
) {
    let class = failure.class;
    let checkpoint = readiness_checkpoint(&publisher.current());
    publish_status(
        publisher,
        ProjectionStatus::Fatal {
            checkpoint,
            error: failure.clone(),
        },
        None,
    );
    counter!("outbe_projection_failures_total", "class" => format!("{class:?}")).increment(1);
    let _ = exit.send(ProjectionExit { failure });
}

fn publish_status(
    publisher: &ProjectionReadinessPublisher,
    status: ProjectionStatus,
    target: Option<FinalizedTarget>,
) {
    let (status_code, ready) = match &status {
        ProjectionStatus::Starting => (0.0, 0.0),
        ProjectionStatus::CatchingUp { .. } => (1.0, 0.0),
        ProjectionStatus::MongoUnavailable { .. } => (2.0, 0.0),
        ProjectionStatus::Ready { .. } => (3.0, 1.0),
        ProjectionStatus::Fatal { .. } => (4.0, 0.0),
    };
    gauge!("outbe_projection_status").set(status_code);
    gauge!("outbe_projection_readiness").set(ready);
    gauge!("outbe_projection_validator_participation_gate").set(ready);
    if let Some(checkpoint) = readiness_checkpoint(&status) {
        gauge!("outbe_projection_checkpoint_number").set(checkpoint.block_number as f64);
        if let Some(target) = target {
            gauge!("outbe_projection_lag_blocks")
                .set(target.number.saturating_sub(checkpoint.block_number) as f64);
        }
    }
    if ready > 0.0 {
        gauge!("outbe_projection_mongo_reconnect_active").set(0.0);
        gauge!("outbe_projection_mongo_reconnect_remaining_seconds").set(0.0);
    }
    publisher.publish(status);
}

fn projection_is_unavailable(error: &eyre::Report) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<StorageError>()
            .is_some_and(|storage| storage.kind() == StorageErrorKind::Unavailable)
    })
}

fn projection_failure_class(error: &eyre::Report) -> ProjectionFailureClass {
    if let Some(storage) = error
        .chain()
        .find_map(|source| source.downcast_ref::<StorageError>())
    {
        return match storage.kind() {
            StorageErrorKind::Corruption => ProjectionFailureClass::CorruptBody,
            StorageErrorKind::WriterLeaseLost => ProjectionFailureClass::WriterLeaseLost,
            StorageErrorKind::InvalidArgument
            | StorageErrorKind::Unavailable
            | StorageErrorKind::Backend
            | StorageErrorKind::RequestDeadline => ProjectionFailureClass::Other,
        };
    }
    if error.chain().any(|source| {
        source
            .downcast_ref::<outbe_offchain_data::ProjectionError>()
            .is_some()
    }) {
        ProjectionFailureClass::MalformedEvent
    } else if error.chain().any(|source| {
        source
            .downcast_ref::<HistoricalProjectionDataError>()
            .is_some()
    }) {
        ProjectionFailureClass::HistoricalReceiptsUnavailable
    } else {
        ProjectionFailureClass::Other
    }
}

fn record_finalized_target(
    latest: &mut Option<FinalizedTarget>,
    pending: &mut Option<FinalizedTarget>,
    incoming: FinalizedTarget,
) -> eyre::Result<bool> {
    match *latest {
        Some(current) if incoming.number < current.number => bail!(
            "finalized target regressed from {} ({}) to {} ({})",
            current.number,
            current.hash,
            incoming.number,
            incoming.hash
        ),
        Some(current) if incoming.number == current.number && incoming.hash != current.hash => {
            bail!(
                "finalized target hash changed at height {}: {} -> {}",
                current.number,
                current.hash,
                incoming.hash
            )
        }
        Some(current) if incoming == current => {
            *pending = Some(incoming);
            Ok(true)
        }
        _ => {
            *latest = Some(incoming);
            *pending = Some(incoming);
            Ok(true)
        }
    }
}

/// During crash recovery Mongo can durably commit finalized block N just before
/// Reth persists its finalized marker for N. Ignore the stale N-1 marker until
/// Reth reaches the already-canonical Mongo checkpoint; never accept a conflict
/// at the checkpoint height.
fn admit_startup_finalized_target(
    startup_floor: &mut Option<FinalizedTarget>,
    incoming: FinalizedTarget,
) -> eyre::Result<bool> {
    let Some(floor) = *startup_floor else {
        return Ok(true);
    };
    if incoming.number < floor.number {
        return Ok(false);
    }
    if incoming.number == floor.number && incoming.hash != floor.hash {
        bail!(
            "finalized target conflicts with recovered projection checkpoint at height {}: {} != {}",
            floor.number,
            incoming.hash,
            floor.hash
        );
    }
    *startup_floor = None;
    Ok(true)
}

fn record_or_publish_finalized_target(
    latest: &mut Option<FinalizedTarget>,
    pending: &mut Option<FinalizedTarget>,
    incoming: FinalizedTarget,
    publisher: &ProjectionReadinessPublisher,
    exit: &tokio::sync::mpsc::UnboundedSender<ProjectionExit>,
) -> bool {
    match record_finalized_target(latest, pending, incoming) {
        Ok(should_attempt) => should_attempt,
        Err(error) => {
            error!(%error, "rejected unsafe finalized projection target");
            publish_fatal(
                publisher,
                exit,
                ProjectionFailureClass::CheckpointMismatch,
                error.to_string(),
            );
            false
        }
    }
}

fn project_through_target<P>(
    provider: P,
    runtime: &Mutex<ProjectionRuntime>,
    target: FinalizedTarget,
    durable_checkpoint_tx: &tokio::sync::mpsc::UnboundedSender<FinalizedTarget>,
    recovery_ack_tx: &tokio::sync::mpsc::UnboundedSender<()>,
) -> eyre::Result<Option<FinalizedTarget>>
where
    P: BlockReader,
{
    // Only one worker is launched at a time. The mutex also makes that ownership explicit and
    // keeps the mutable projector state available across retry attempts.
    let mut runtime = runtime
        .lock()
        .map_err(|_| eyre::eyre!("offchain-data projector lock is poisoned"))?;
    runtime.projector = OffchainDataProjection::open(
        runtime.projection_config,
        runtime.reader.clone(),
        runtime.writer.clone(),
    )
    .wrap_err("reload durable offchain-data projector state")?;
    runtime
        .writer
        .verify_transaction_capability()
        .wrap_err("verify recovered MongoDB transaction capability")?;
    let projector = &mut runtime.projector;
    let state = projector.state();
    let checkpoint = state.checkpoint;
    let start_block = state.start_block;

    if let Some(checkpoint) = checkpoint {
        let canonical_hash = provider
            .block_hash(checkpoint.block_number)
            .wrap_err_with(|| {
                format!(
                    "load canonical hash for restored projection checkpoint {}",
                    checkpoint.block_number
                )
            })?
            .ok_or_else(|| {
                eyre::eyre!(
                    "canonical block {} for restored projection checkpoint is unavailable",
                    checkpoint.block_number
                )
            })?;
        if canonical_hash != checkpoint.block_hash {
            bail!(
                "projection checkpoint hash {} conflicts with canonical hash {} at height {}",
                checkpoint.block_hash,
                canonical_hash,
                checkpoint.block_number
            );
        }
    }
    recovery_ack_tx
        .send(())
        .map_err(|_| eyre::eyre!("projection recovery acknowledgement receiver is closed"))?;

    let first_block = match checkpoint {
        Some(checkpoint) if checkpoint.block_number > target.number => bail!(
            "projection checkpoint {} ({}) is ahead of finalized target {} ({})",
            checkpoint.block_number,
            checkpoint.block_hash,
            target.number,
            target.hash
        ),
        Some(checkpoint)
            if checkpoint.block_number == target.number && checkpoint.block_hash != target.hash =>
        {
            bail!(
                "projection checkpoint hash {} conflicts with finalized hash {} at height {}",
                checkpoint.block_hash,
                target.hash,
                target.number
            )
        }
        Some(checkpoint) if checkpoint.block_number == target.number => {
            let checkpoint = FinalizedTarget::new(checkpoint.block_number, checkpoint.block_hash);
            durable_checkpoint_tx
                .send(checkpoint)
                .map_err(|_| eyre::eyre!("durable checkpoint receiver is closed"))?;
            return Ok(Some(checkpoint));
        }
        Some(checkpoint) => checkpoint
            .block_number
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("projection checkpoint height overflow"))?,
        None => start_block,
    };

    if first_block > target.number {
        // A fresh projector intentionally does no work before its configured start height. There
        // is no durable checkpoint yet, so the caller must not emit FinishedHeight.
        return Ok(None);
    }

    let mut durable_checkpoint = None;
    for block_number in first_block..=target.number {
        let canonical_hash = provider
            .block_hash(block_number)
            .wrap_err_with(|| format!("load canonical hash for block {block_number}"))?
            .ok_or(HistoricalProjectionDataError::CanonicalBlock { block_number })?;
        let block = provider
            .block_by_hash(canonical_hash)
            .wrap_err_with(|| format!("load canonical block {block_number} ({canonical_hash})"))?
            .ok_or(HistoricalProjectionDataError::CanonicalBlockByHash {
                block_number,
                block_hash: canonical_hash,
            })?;

        if block.header().number() != block_number {
            bail!(
                "provider returned block {} while canonical block {} was requested",
                block.header().number(),
                block_number
            );
        }
        let block_hash = block.header().hash_slow();
        if block_hash != canonical_hash {
            bail!(
                "block loaded for canonical hash {} recomputed to {} at height {}",
                canonical_hash,
                block_hash,
                block_number
            );
        }
        if block_number == target.number && block_hash != target.hash {
            bail!(
                "canonical block hash {} conflicts with finalized hash {} at height {}",
                block_hash,
                target.hash,
                block_number
            );
        }

        let receipts = provider
            .receipts_by_block(block_hash.into())
            .wrap_err_with(|| format!("load receipts for canonical block {block_number}"))?
            .ok_or(HistoricalProjectionDataError::Receipts { block_number })?;
        let transactions = block.body().transactions();
        if transactions.len() != receipts.len() {
            bail!(
                "canonical block {} has {} transactions but {} receipts",
                block_number,
                transactions.len(),
                receipts.len()
            );
        }

        let mut normalized_receipts = Vec::with_capacity(receipts.len());
        let mut next_log_index = 0_u64;
        for (transaction_index, (transaction, receipt)) in
            transactions.iter().zip(receipts).enumerate()
        {
            let transaction_index = u64::try_from(transaction_index)
                .map_err(|_| eyre::eyre!("transaction index does not fit u64"))?;
            let mut logs = Vec::with_capacity(receipt.logs().len());
            for log in receipt.logs() {
                logs.push(FinalizedLog {
                    log_index: next_log_index,
                    emitter: log.address,
                    data: log.data.clone(),
                });
                next_log_index = next_log_index
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("block-global log index overflow"))?;
            }
            normalized_receipts.push(FinalizedReceipt {
                tx_hash: *transaction.tx_hash(),
                transaction_index,
                success: receipt.status(),
                // Every log is retained in receipt order, including unrelated logs, so these
                // indices remain the canonical block-global indices.
                logs,
            });
        }

        let projected = projector
            .project_block(&FinalizedBlock {
                number: block_number,
                hash: block_hash,
                receipts: normalized_receipts,
            })
            .wrap_err_with(|| format!("project finalized block {block_number}"))?;
        let projected = match projected {
            ProjectionOutcome::Applied { checkpoint, .. }
            | ProjectionOutcome::AlreadyApplied(checkpoint) => checkpoint,
        };
        if projected.block_number != block_number || projected.block_hash != block_hash {
            bail!(
                "projector returned checkpoint {} ({}) after projecting {} ({})",
                projected.block_number,
                projected.block_hash,
                block_number,
                block_hash
            );
        }
        durable_checkpoint = Some(FinalizedTarget::new(
            projected.block_number,
            projected.block_hash,
        ));
        durable_checkpoint_tx
            .send(FinalizedTarget::new(
                projected.block_number,
                projected.block_hash,
            ))
            .map_err(|_| eyre::eyre!("durable checkpoint receiver is closed"))?;
    }

    Ok(durable_checkpoint)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use super::{
        prepare_offchain_data_projection, project_through_target, projection_failure_class,
        record_finalized_target, record_or_publish_finalized_target, require_finalized_checkpoint,
        run_projection_loop, spawn_detached_projection_work, supervise_projection_future,
        FinalizedTarget, OffchainDataProjectionConfig, ProjectionRuntime, MONGO_RECONNECT_DEADLINE,
    };
    use alloy_consensus::Header;
    use alloy_primitives::B256;
    use outbe_offchain_data::{
        OffchainDataProjection, ProjectionConfig, ProjectionFailure, ProjectionFailureClass,
    };
    use outbe_offchain_storage::{
        AtomicWriteBatch, Key, MemoryStorage, Namespace, ScanPage, ScanRequest, StorageError,
        StorageReader, StorageReaderHandle, StorageWriter, StorageWriterHandle, StoredValue,
    };
    use reth_ethereum::{exex::ExExEvent, Block};
    use reth_provider::test_utils::MockEthProvider;

    use outbe_primitives::projection::{projection_readiness, ProjectionCheckpoint};

    #[test]
    fn finalized_targets_coalesce_to_the_latest_height() {
        let first = FinalizedTarget::new(10, B256::repeat_byte(1));
        let second = FinalizedTarget::new(12, B256::repeat_byte(2));
        let mut latest = Some(first);
        let mut pending = Some(first);

        assert!(record_finalized_target(&mut latest, &mut pending, second).unwrap());
        assert_eq!(latest, Some(second));
        assert_eq!(pending, Some(second));
    }

    #[test]
    fn startup_rejects_unavailable_mongodb_before_exex_runs() {
        let started = std::time::Instant::now();
        drop(
            prepare_offchain_data_projection(OffchainDataProjectionConfig {
                chain_id: outbe_primitives::chain::DEVNET_CHAIN_ID,
                genesis_hash: B256::repeat_byte(0x11),
                start_block: 1,
                mongodb_uri:
                    "mongodb://127.0.0.1:1/?directConnection=true&serverSelectionTimeoutMS=50"
                        .to_owned(),
                mongodb_database: "startup_unavailable".to_owned(),
            })
            .err()
            .expect("unavailable MongoDB must fail startup preparation"),
        );
        assert!(
            started.elapsed() >= MONGO_RECONNECT_DEADLINE,
            "startup returned before the shared reconnect deadline"
        );
        assert!(
            started.elapsed() <= MONGO_RECONNECT_DEADLINE + Duration::from_millis(250),
            "startup exceeded the shared reconnect deadline"
        );
    }

    #[test]
    fn finalized_target_regression_is_rejected() {
        let current = FinalizedTarget::new(10, B256::repeat_byte(1));
        let mut latest = Some(current);
        let mut pending = Some(current);

        let error = record_finalized_target(
            &mut latest,
            &mut pending,
            FinalizedTarget::new(9, B256::repeat_byte(2)),
        )
        .unwrap_err();

        assert!(error.to_string().contains("regressed"));
        assert_eq!(latest, Some(current));
        assert_eq!(pending, Some(current));
    }

    #[test]
    fn conflicting_hash_at_same_finalized_height_is_rejected() {
        let current = FinalizedTarget::new(10, B256::repeat_byte(1));
        let mut latest = Some(current);
        let mut pending = Some(current);

        let error = record_finalized_target(
            &mut latest,
            &mut pending,
            FinalizedTarget::new(10, B256::repeat_byte(2)),
        )
        .unwrap_err();

        assert!(error.to_string().contains("hash changed"));
        assert_eq!(latest, Some(current));
        assert_eq!(pending, Some(current));
    }

    #[test]
    fn finalized_target_conflict_publishes_fatal_exit_on_every_ingress_path() {
        let current = FinalizedTarget::new(10, B256::repeat_byte(1));
        let mut latest = Some(current);
        let mut pending = None;
        let checkpoint = ProjectionCheckpoint {
            block_number: current.number,
            block_hash: current.hash,
        };
        let (publisher, readiness) = projection_readiness(
            checkpoint,
            outbe_offchain_data::ProjectionStatus::Ready { checkpoint },
        );
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(!record_or_publish_finalized_target(
            &mut latest,
            &mut pending,
            FinalizedTarget::new(10, B256::repeat_byte(2)),
            &publisher,
            &exit_tx,
        ));
        assert!(matches!(
            readiness.current(),
            outbe_offchain_data::ProjectionStatus::Fatal { error, .. }
                if error.class == ProjectionFailureClass::CheckpointMismatch
        ));
        assert_eq!(
            exit_rx.try_recv().unwrap().failure.class,
            ProjectionFailureClass::CheckpointMismatch,
        );
    }

    #[test]
    fn startup_checkpoint_floor_ignores_stale_finality_then_releases() {
        let checkpoint = FinalizedTarget::new(4, B256::repeat_byte(0x44));
        let mut floor = Some(checkpoint);

        assert!(!super::admit_startup_finalized_target(
            &mut floor,
            FinalizedTarget::new(3, B256::repeat_byte(0x33)),
        )
        .unwrap());
        assert_eq!(floor, Some(checkpoint));

        let error = super::admit_startup_finalized_target(
            &mut floor,
            FinalizedTarget::new(4, B256::repeat_byte(0x45)),
        )
        .unwrap_err();
        assert!(error.to_string().contains("conflicts"));
        assert_eq!(floor, Some(checkpoint));

        assert!(super::admit_startup_finalized_target(&mut floor, checkpoint).unwrap());
        assert_eq!(floor, None);
    }

    #[test]
    fn persisted_checkpoint_requires_an_actual_reth_finalized_identity() {
        let checkpoint = ProjectionCheckpoint {
            block_number: 4,
            block_hash: B256::repeat_byte(0x44),
        };
        let error = require_finalized_checkpoint(checkpoint, None).unwrap_err();
        assert!(error.to_string().contains("before local Reth finality"));

        assert_eq!(
            require_finalized_checkpoint(
                checkpoint,
                Some(FinalizedTarget::new(3, B256::repeat_byte(0x33))),
            )
            .expect("one-block crash-consistency gap must recover"),
            FinalizedTarget::new(3, B256::repeat_byte(0x33)),
        );

        let error = require_finalized_checkpoint(
            checkpoint,
            Some(FinalizedTarget::new(2, B256::repeat_byte(0x22))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("ahead of local Reth finalized"));

        let error = require_finalized_checkpoint(
            checkpoint,
            Some(FinalizedTarget::new(4, B256::repeat_byte(0x45))),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not match local Reth finalized"));

        assert_eq!(
            require_finalized_checkpoint(
                checkpoint,
                Some(FinalizedTarget::new(4, checkpoint.block_hash)),
            )
            .unwrap(),
            FinalizedTarget::new(4, checkpoint.block_hash),
        );
    }

    #[test]
    fn dropping_projection_waiter_never_waits_for_blocked_backend_work() {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let receiver = spawn_detached_projection_work("projection-shutdown-test", move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        })
        .unwrap();
        started_rx.recv().unwrap();

        let started = std::time::Instant::now();
        drop(receiver);
        assert!(started.elapsed() < Duration::from_millis(50));
        release_tx.send(()).unwrap();
    }

    #[test]
    fn projects_each_intermediate_block_and_reports_each_durable_checkpoint() {
        let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
        let first = add_empty_block(&provider, 1);
        let second = add_empty_block(&provider, 2);
        let runtime = initialized_runtime(1);
        let (checkpoint_tx, mut checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recovery_tx, _recovery_rx) = tokio::sync::mpsc::unbounded_channel();

        let result = project_through_target(
            provider,
            &runtime,
            FinalizedTarget::new(2, second),
            &checkpoint_tx,
            &recovery_tx,
        )
        .unwrap();

        assert_eq!(result, Some(FinalizedTarget::new(2, second)));
        assert_eq!(
            checkpoint_rx.try_recv().unwrap(),
            FinalizedTarget::new(1, first)
        );
        assert_eq!(
            checkpoint_rx.try_recv().unwrap(),
            FinalizedTarget::new(2, second)
        );
        assert!(checkpoint_rx.try_recv().is_err());
        let state = runtime.lock().unwrap();
        let checkpoint = state.projector.state().checkpoint.unwrap();
        assert_eq!(checkpoint.block_number, 2);
        assert_eq!(checkpoint.block_hash, second);
    }

    #[test]
    fn later_provider_failure_keeps_and_reports_earlier_durable_checkpoint() {
        let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
        let first = add_empty_block(&provider, 1);
        let runtime = initialized_runtime(1);
        let (checkpoint_tx, mut checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();
        let (recovery_tx, _recovery_rx) = tokio::sync::mpsc::unbounded_channel();

        let error = project_through_target(
            provider,
            &runtime,
            FinalizedTarget::new(2, B256::repeat_byte(2)),
            &checkpoint_tx,
            &recovery_tx,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("canonical block 2 is unavailable"));
        assert_eq!(
            projection_failure_class(&error),
            ProjectionFailureClass::HistoricalReceiptsUnavailable
        );
        assert_eq!(
            checkpoint_rx.try_recv().unwrap(),
            FinalizedTarget::new(1, first)
        );
        assert!(checkpoint_rx.try_recv().is_err());
        let state = runtime.lock().unwrap();
        let checkpoint = state.projector.state().checkpoint.unwrap();
        assert_eq!(checkpoint.block_number, 1);
        assert_eq!(checkpoint.block_hash, first);
    }

    #[test]
    fn writer_lease_loss_has_a_distinct_failure_class() {
        let error = eyre::Report::new(StorageError::WriterLeaseLost);

        assert_eq!(
            projection_failure_class(&error),
            ProjectionFailureClass::WriterLeaseLost
        );
    }

    #[tokio::test]
    async fn control_loop_drains_notifications_and_emits_finished_heights_in_order() {
        use futures::{channel::mpsc, SinkExt};

        let provider = MockEthProvider::new();
        let first = add_empty_block(&provider, 1);
        let second = add_empty_block(&provider, 2);
        let runtime = initialized_runtime(1).into_inner().unwrap();
        let (mut notification_tx, notification_rx) = mpsc::channel(1);
        let (finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (exit_tx, _exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            runtime,
            exit_tx,
        ));

        notification_tx.send(Ok(())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), notification_tx.send(Ok(())))
            .await
            .unwrap()
            .unwrap();
        finality_tx
            .unbounded_send(FinalizedTarget::new(2, second))
            .unwrap();

        let first_event = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let second_event = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_event, ExExEvent::FinishedHeight((1, first).into()));
        assert_eq!(second_event, ExExEvent::FinishedHeight((2, second).into()));
        assert!(
            !task.is_finished(),
            "the critical projection loop stays alive"
        );
        task.abort();
    }

    #[tokio::test]
    async fn deterministic_projection_failure_reports_exit_while_exex_keeps_draining() {
        use futures::{channel::mpsc, SinkExt};

        let provider = MockEthProvider::new();
        let block_hash = add_empty_block(&provider, 1);
        let storage = Arc::new(FailAfterStartupStorage::default());
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage.clone();
        let projection_config = ProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block: 1,
        };
        let projector =
            OffchainDataProjection::open(projection_config, reader.clone(), writer.clone())
                .unwrap();
        storage.fail_writes.store(true, Ordering::SeqCst);

        let (mut notification_tx, notification_rx) = mpsc::channel(1);
        let (finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (readiness_publisher, _readiness) = outbe_offchain_data::projection_readiness(
            outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: B256::repeat_byte(0x11),
            },
            outbe_offchain_data::ProjectionStatus::Starting,
        );
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            ProjectionRuntime {
                projector,
                readiness_publisher,
                projection_config,
                reader,
                writer,
                writer_lease: None,
                runtime_failure_receiver: Some(runtime_failure_rx),
            },
            exit_tx,
        ));
        finality_tx
            .unbounded_send(FinalizedTarget::new(1, block_hash))
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while storage.failed_writes.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("projection worker must observe the injected storage failure");
        assert!(events_rx.try_recv().is_err());
        let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
            .await
            .expect("fatal projection failure must notify the node supervisor")
            .expect("projection exit channel must remain open");
        assert_eq!(
            exit.failure.class,
            outbe_offchain_data::ProjectionFailureClass::CorruptBody
        );

        notification_tx.send(Ok(())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), notification_tx.send(Ok(())))
            .await
            .expect("ExEx must keep draining notifications after projection failure")
            .unwrap();
        assert!(
            !task.is_finished(),
            "projection failure must not stop the node loop"
        );
        task.abort();
    }

    #[tokio::test]
    async fn runtime_body_corruption_reports_exit_while_exex_keeps_draining() {
        use futures::{channel::mpsc, SinkExt};

        let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
        let storage = Arc::new(MemoryStorage::new());
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage;
        let projection_config = ProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block: 1,
        };
        let projector =
            OffchainDataProjection::open(projection_config, reader.clone(), writer.clone())
                .unwrap();
        let (readiness_publisher, _readiness) = outbe_offchain_data::projection_readiness(
            outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: projection_config.genesis_hash,
            },
            outbe_offchain_data::ProjectionStatus::Starting,
        );
        let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
        let (mut notification_tx, notification_rx) = mpsc::channel(1);
        let (_finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            ProjectionRuntime {
                projector,
                readiness_publisher,
                projection_config,
                reader,
                writer,
                writer_lease: None,
                runtime_failure_receiver: Some(runtime_failure_rx),
            },
            exit_tx,
        ));

        runtime_failure_tx.send_replace(Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(
            outbe_offchain_data::ProjectionFailure::new(
                outbe_offchain_data::ProjectionFailureClass::CorruptBody,
                "dangling body index",
            ),
        )));
        let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
            .await
            .expect("body corruption must notify the node supervisor")
            .expect("projection exit channel must remain open");
        assert_eq!(
            exit.failure.class,
            outbe_offchain_data::ProjectionFailureClass::CorruptBody
        );

        notification_tx.send(Ok(())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), notification_tx.send(Ok(())))
            .await
            .expect("ExEx must keep draining after a fatal runtime read")
            .unwrap();
        assert!(!task.is_finished());
        task.abort();
    }

    #[tokio::test]
    async fn unexpected_exex_return_reports_fatal_and_stays_alive_for_common_shutdown() {
        let (publisher, readiness) = outbe_offchain_data::projection_readiness(
            outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: B256::repeat_byte(0x11),
            },
            outbe_offchain_data::ProjectionStatus::Starting,
        );
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(supervise_projection_future(
            async { Ok(()) },
            publisher,
            exit_tx,
        ));

        let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
            .await
            .expect("unexpected return must notify lifecycle owner")
            .expect("exit sender must remain open");
        assert_eq!(
            exit.failure.class,
            outbe_offchain_data::ProjectionFailureClass::ProjectorExited
        );
        assert!(matches!(
            readiness.current(),
            outbe_offchain_data::ProjectionStatus::Fatal { error, .. }
                if error.class == outbe_offchain_data::ProjectionFailureClass::ProjectorExited
        ));
        assert!(!task.is_finished());
        task.abort();
    }

    #[tokio::test]
    async fn runtime_body_unavailability_uses_the_projection_recovery_session() {
        use futures::channel::mpsc;

        let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new();
        let storage = Arc::new(FailAfterStartupStorage::default());
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage.clone();
        let projection_config = ProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block: 1,
        };
        let projector =
            OffchainDataProjection::open(projection_config, reader.clone(), writer.clone())
                .unwrap();
        let (readiness_publisher, readiness) = outbe_offchain_data::projection_readiness(
            outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: projection_config.genesis_hash,
            },
            outbe_offchain_data::ProjectionStatus::Ready {
                checkpoint: outbe_offchain_data::ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: projection_config.genesis_hash,
                },
            },
        );
        let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
        let (_notification_tx, notification_rx) = mpsc::channel(1);
        let (_finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            ProjectionRuntime {
                projector,
                readiness_publisher,
                projection_config,
                reader,
                writer,
                writer_lease: None,
                runtime_failure_receiver: Some(runtime_failure_rx),
            },
            exit_tx,
        ));

        storage.fail_reads.store(true, Ordering::SeqCst);
        runtime_failure_tx.send_replace(Some(outbe_offchain_data::RuntimeBodyFailure::Unavailable));
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    readiness.current(),
                    outbe_offchain_data::ProjectionStatus::MongoUnavailable { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("read-side outage must immediately disable readiness");
        assert!(exit_rx.try_recv().is_err());

        storage.fail_reads.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    readiness.current(),
                    outbe_offchain_data::ProjectionStatus::Ready { checkpoint }
                        if checkpoint.block_number == 0
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an acknowledged projector reopen must restore readiness");
        assert!(exit_rx.try_recv().is_err());
        task.abort();
    }

    #[tokio::test]
    async fn unavailable_projection_write_retries_and_advances_only_after_recovery() {
        use futures::channel::mpsc;

        let provider = MockEthProvider::new();
        let block_hash = add_empty_block(&provider, 1);
        let storage = Arc::new(FailAfterStartupStorage::default());
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage.clone();
        let projection_config = ProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block: 1,
        };
        let projector =
            OffchainDataProjection::open(projection_config, reader.clone(), writer.clone())
                .unwrap();
        let (readiness_publisher, readiness) = outbe_offchain_data::projection_readiness(
            outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: projection_config.genesis_hash,
            },
            outbe_offchain_data::ProjectionStatus::Ready {
                checkpoint: outbe_offchain_data::ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: projection_config.genesis_hash,
                },
            },
        );
        let (_runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
        let (_notification_tx, notification_rx) = mpsc::channel(1);
        let (finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            ProjectionRuntime {
                projector,
                readiness_publisher,
                projection_config,
                reader,
                writer,
                writer_lease: None,
                runtime_failure_receiver: Some(runtime_failure_rx),
            },
            exit_tx,
        ));

        storage
            .fail_writes_unavailable
            .store(true, Ordering::SeqCst);
        finality_tx
            .unbounded_send(FinalizedTarget::new(1, block_hash))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    readiness.current(),
                    outbe_offchain_data::ProjectionStatus::MongoUnavailable { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed receipt transaction must enter recovery");
        assert!(events_rx.try_recv().is_err());

        storage
            .fail_writes_unavailable
            .store(false, Ordering::SeqCst);
        let event = tokio::time::timeout(Duration::from_secs(2), events_rx.recv())
            .await
            .expect("projection must retry after the one-second interval")
            .expect("ExEx event sender remains live");
        assert_eq!(event, ExExEvent::FinishedHeight((1, block_hash).into()));
        assert!(matches!(
            readiness.current(),
            outbe_offchain_data::ProjectionStatus::Ready { checkpoint }
                if checkpoint.block_number == 1 && checkpoint.block_hash == block_hash
        ));
        assert!(exit_rx.try_recv().is_err());
        assert!(!task.is_finished());
        task.abort();
    }

    #[tokio::test]
    async fn fatal_status_stays_sticky_when_detached_worker_finishes_late() {
        use futures::channel::mpsc;

        let provider = MockEthProvider::new();
        let block_hash = add_empty_block(&provider, 1);
        let (storage, write_started, release_write, write_finished) = BlockingWriteStorage::new();
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage.clone();
        let projection_config = ProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block: 1,
        };
        let projector =
            OffchainDataProjection::open(projection_config, reader.clone(), writer.clone())
                .unwrap();
        let checkpoint = ProjectionCheckpoint {
            block_number: 0,
            block_hash: projection_config.genesis_hash,
        };
        let (readiness_publisher, readiness) = projection_readiness(
            checkpoint,
            outbe_offchain_data::ProjectionStatus::Ready { checkpoint },
        );
        let (runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
        let (_notification_tx, notification_rx) = mpsc::channel(1);
        let (finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        storage.block_next_write.store(true, Ordering::Release);
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            ProjectionRuntime {
                projector,
                readiness_publisher,
                projection_config,
                reader,
                writer,
                writer_lease: None,
                runtime_failure_receiver: Some(runtime_failure_rx),
            },
            exit_tx,
        ));

        finality_tx
            .unbounded_send(FinalizedTarget::new(1, block_hash))
            .unwrap();
        tokio::task::spawn_blocking(move || write_started.recv().unwrap())
            .await
            .unwrap();
        runtime_failure_tx.send_replace(Some(outbe_offchain_data::RuntimeBodyFailure::Fatal(
            ProjectionFailure::new(ProjectionFailureClass::Other, "injected terminal failure"),
        )));
        let exit = tokio::time::timeout(Duration::from_secs(1), exit_rx.recv())
            .await
            .expect("fatal body-read failure must reach the lifecycle owner")
            .expect("exit channel remains open");
        assert_eq!(exit.failure.class, ProjectionFailureClass::Other);

        release_write.send(()).unwrap();
        tokio::task::spawn_blocking(move || write_finished.recv().unwrap())
            .await
            .unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            readiness.current(),
            outbe_offchain_data::ProjectionStatus::Fatal { error, .. }
                if error.class == ProjectionFailureClass::Other
        ));
        assert!(events_rx.try_recv().is_err());
        assert!(!task.is_finished());
        task.abort();
    }

    fn add_empty_block(provider: &MockEthProvider, number: u64) -> B256 {
        let header = Header {
            number,
            ..Default::default()
        };
        let hash = header.hash_slow();
        provider.add_block(hash, Block::new(header, Default::default()));
        provider.add_receipts(number, Vec::new());
        hash
    }

    fn initialized_runtime(start_block: u64) -> Mutex<ProjectionRuntime> {
        let storage = Arc::new(MemoryStorage::new());
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage;
        let projection_config = ProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block,
        };
        let projector =
            OffchainDataProjection::open(projection_config, reader.clone(), writer.clone())
                .unwrap();
        let (readiness_publisher, _readiness) = outbe_offchain_data::projection_readiness(
            outbe_offchain_data::ProjectionCheckpoint {
                block_number: 0,
                block_hash: B256::repeat_byte(0x11),
            },
            outbe_offchain_data::ProjectionStatus::Starting,
        );
        let (_runtime_failure_tx, runtime_failure_rx) = tokio::sync::watch::channel(None);
        Mutex::new(ProjectionRuntime {
            projector,
            readiness_publisher,
            projection_config,
            reader,
            writer,
            writer_lease: None,
            runtime_failure_receiver: Some(runtime_failure_rx),
        })
    }

    struct BlockingWriteStorage {
        inner: MemoryStorage,
        block_next_write: AtomicBool,
        write_started: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
        release_write: Mutex<std::sync::mpsc::Receiver<()>>,
        write_finished: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    }

    impl BlockingWriteStorage {
        fn new() -> (
            Arc<Self>,
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        ) {
            let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
            (
                Arc::new(Self {
                    inner: MemoryStorage::new(),
                    block_next_write: AtomicBool::new(false),
                    write_started: Mutex::new(Some(started_tx)),
                    release_write: Mutex::new(release_rx),
                    write_finished: Mutex::new(Some(finished_tx)),
                }),
                started_rx,
                release_tx,
                finished_rx,
            )
        }
    }

    impl StorageReader for BlockingWriteStorage {
        fn get_record(
            &self,
            namespace: Namespace,
            key: &Key,
        ) -> Result<Option<StoredValue>, StorageError> {
            self.inner.get_record(namespace, key)
        }

        fn get_records(
            &self,
            namespace: Namespace,
            keys: &[Key],
        ) -> Result<Vec<Option<StoredValue>>, StorageError> {
            self.inner.get_records(namespace, keys)
        }

        fn scan_prefix(
            &self,
            namespace: Namespace,
            request: ScanRequest<'_>,
        ) -> Result<ScanPage, StorageError> {
            self.inner.scan_prefix(namespace, request)
        }
    }

    impl StorageWriter for BlockingWriteStorage {
        fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
            let blocked = self.block_next_write.swap(false, Ordering::AcqRel);
            if blocked {
                if let Some(started) = self.write_started.lock().unwrap().take() {
                    let _ = started.send(());
                }
                self.release_write.lock().unwrap().recv().unwrap();
            }
            let result = self.inner.apply_atomic(batch);
            if blocked {
                if let Some(finished) = self.write_finished.lock().unwrap().take() {
                    let _ = finished.send(());
                }
            }
            result
        }
    }

    #[derive(Debug, Default)]
    struct FailAfterStartupStorage {
        inner: MemoryStorage,
        fail_reads: AtomicBool,
        fail_writes: AtomicBool,
        fail_writes_unavailable: AtomicBool,
        failed_writes: AtomicUsize,
    }

    impl StorageReader for FailAfterStartupStorage {
        fn get_record(
            &self,
            namespace: Namespace,
            key: &Key,
        ) -> Result<Option<StoredValue>, StorageError> {
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(StorageError::Unavailable {
                    source: std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "injected unavailable read",
                    )
                    .into(),
                });
            }
            self.inner.get_record(namespace, key)
        }

        fn get_records(
            &self,
            namespace: Namespace,
            keys: &[Key],
        ) -> Result<Vec<Option<StoredValue>>, StorageError> {
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(StorageError::Unavailable {
                    source: std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "injected unavailable read",
                    )
                    .into(),
                });
            }
            self.inner.get_records(namespace, keys)
        }

        fn scan_prefix(
            &self,
            namespace: Namespace,
            request: ScanRequest<'_>,
        ) -> Result<ScanPage, StorageError> {
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(StorageError::Unavailable {
                    source: std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "injected unavailable read",
                    )
                    .into(),
                });
            }
            self.inner.scan_prefix(namespace, request)
        }
    }

    impl StorageWriter for FailAfterStartupStorage {
        fn verify_transaction_capability(&self) -> Result<(), StorageError> {
            if self.fail_reads.load(Ordering::SeqCst)
                || self.fail_writes_unavailable.load(Ordering::SeqCst)
            {
                return Err(StorageError::Unavailable {
                    source: Box::new(std::io::Error::other(
                        "injected unavailable transaction capability",
                    )),
                });
            }
            Ok(())
        }

        fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
            if self.fail_writes_unavailable.load(Ordering::SeqCst) {
                self.failed_writes.fetch_add(1, Ordering::SeqCst);
                return Err(StorageError::Unavailable {
                    source: Box::new(std::io::Error::other(
                        "injected unavailable projection write",
                    )),
                });
            }
            if self.fail_writes.load(Ordering::SeqCst) {
                self.failed_writes.fetch_add(1, Ordering::SeqCst);
                return Err(StorageError::Corruption(
                    "injected post-startup deterministic failure".to_owned(),
                ));
            }
            self.inner.apply_atomic(batch)
        }
    }
}
