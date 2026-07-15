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
use futures::{Stream, StreamExt};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
    ProjectionOutcome,
};
use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
use reth_chain_state::ForkChoiceSubscriptions;
use reth_ethereum::exex::{ExExContext, ExExEvent};
use reth_node_builder::FullNodeComponents;
use reth_primitives_traits::{Block, BlockBody};
use reth_provider::{BlockHashReader, BlockIdReader, BlockReader};
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tracing::{error, info, warn};

const PROJECTION_RETRY_INTERVAL: Duration = Duration::from_secs(5);

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

type ProjectionAttempt = JoinHandle<eyre::Result<Option<FinalizedTarget>>>;

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
}

/// Projection instance whose available canonical checkpoint identity passed startup checks.
pub struct ReadyOffchainDataProjection {
    projector: OffchainDataProjection,
}

/// Connects to MongoDB and validates storage prerequisites before Reth component initialization.
pub fn prepare_offchain_data_projection(
    config: OffchainDataProjectionConfig,
) -> eyre::Result<PreparedOffchainDataProjection> {
    let projection_config = ProjectionConfig {
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        start_block: config.start_block,
    };
    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri: config.mongodb_uri,
            database: config.mongodb_database,
        })
        .wrap_err("configure offchain-data MongoDB")?,
    );
    storage
        .verify_transaction_support()
        .wrap_err("validate offchain-data MongoDB transaction support")?;
    let projector = OffchainDataProjection::open(projection_config, storage.clone(), storage)
        .wrap_err("validate offchain-data MongoDB state")?;
    Ok(PreparedOffchainDataProjection { projector })
}

/// Validates a persisted checkpoint against canonical Reth state during ExEx initialization.
pub fn validate_offchain_data_checkpoint<P>(
    prepared: PreparedOffchainDataProjection,
    canonical_hashes: &P,
) -> eyre::Result<ReadyOffchainDataProjection>
where
    P: BlockHashReader,
{
    let projector = prepared.projector;
    if let Some(checkpoint) = projector.state().checkpoint {
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
            None => warn!(
                checkpoint_number = checkpoint.block_number,
                checkpoint_hash = %checkpoint.block_hash,
                "canonical checkpoint block is not available yet; validation remains pending until Reth synchronization"
            ),
        }
    }
    let projection_state = projector.state();
    info!(
        chain_id = projection_state.chain_id,
        genesis_hash = %projection_state.genesis_hash,
        start_block = projection_state.start_block,
        "finalized offchain-data projection ready"
    );
    Ok(ReadyOffchainDataProjection { projector })
}

struct ProjectionRuntime {
    projector: OffchainDataProjection,
}

impl ProjectionRuntime {
    fn new(ready: ReadyOffchainDataProjection) -> Self {
        Self {
            projector: ready.projector,
        }
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
    )
    .await
}

async fn run_projection_loop<P, N, F>(
    provider: P,
    mut notifications: N,
    mut finalized_blocks: F,
    events: tokio::sync::mpsc::UnboundedSender<ExExEvent>,
    runtime: ProjectionRuntime,
) -> eyre::Result<()>
where
    P: BlockIdReader + BlockReader + Clone + Send + 'static,
    N: Stream<Item = Result<(), String>> + Unpin,
    F: Stream<Item = FinalizedTarget> + Unpin,
{
    let start_block = runtime.projector.state().start_block;
    let projector = Arc::new(Mutex::new(runtime));
    let (durable_checkpoint_tx, mut durable_checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();

    // `finalized_block_stream` emits only changes, so the current provider value must be sampled
    // separately to avoid waiting forever when the node starts at an already-finalized height.
    let initial_target = match provider.finalized_block_num_hash() {
        Ok(block) => block.map(|block| FinalizedTarget::new(block.number, block.hash)),
        Err(error) => {
            warn!(%error, "failed to sample current finalized block; retrying later");
            None
        }
    };

    let mut latest_target = initial_target;
    let mut pending_target = initial_target;
    let mut projection_attempt: Option<ProjectionAttempt> = None;
    let mut can_start_attempt = true;
    let mut finality_stalled = false;
    let mut notifications_open = true;
    let mut finalized_stream_open = true;

    let retry_start = tokio::time::Instant::now() + PROJECTION_RETRY_INTERVAL;
    let mut retry = tokio::time::interval_at(retry_start, PROJECTION_RETRY_INTERVAL);
    retry.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        if projection_attempt.is_none() && can_start_attempt && !finality_stalled {
            if let Some(target) = pending_target {
                let provider = provider.clone();
                let projector = Arc::clone(&projector);
                let durable_checkpoint_tx = durable_checkpoint_tx.clone();
                projection_attempt = Some(tokio::task::spawn_blocking(move || {
                    project_through_target(provider, &projector, target, &durable_checkpoint_tx)
                }));
                can_start_attempt = false;
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
                        match record_finalized_target(&mut latest_target, &mut pending_target, target) {
                            Ok(should_attempt) => {
                                can_start_attempt |= should_attempt;
                            }
                            Err(error) => {
                                // Conflicting or regressing finality is unsafe to project, but the
                                // task must stay alive and continue draining notifications.
                                error!(%error, "rejected unsafe finalized projection target");
                                can_start_attempt = false;
                                finality_stalled = true;
                            }
                        }
                    }
                    None => {
                        finalized_stream_open = false;
                        warn!("offchain-data finalized block stream closed");
                    }
                }
            }

            result = async {
                match projection_attempt.as_mut() {
                    Some(attempt) => attempt.await,
                    None => std::future::pending().await,
                }
            }, if projection_attempt.is_some() => {
                projection_attempt = None;
                match result {
                    Ok(Ok(durable_checkpoint)) => {
                        if pending_target.is_some_and(|pending| {
                            durable_checkpoint.map_or(
                                pending.number < start_block,
                                |checkpoint| pending.number <= checkpoint.number,
                            )
                        }) {
                            pending_target = None;
                        }
                        can_start_attempt = pending_target.is_some();
                    }
                    Ok(Err(error)) => {
                        error!(%error, "finalized offchain-data projection stalled; retrying later");
                        // Keep the target pending. The timer or a newer finalized target will
                        // permit one later retry; never spin a blocking worker in a tight loop.
                        can_start_attempt = false;
                    }
                    Err(error) => {
                        error!(%error, "finalized offchain-data projection worker failed; retrying later");
                        can_start_attempt = false;
                    }
                }
            }

            checkpoint = durable_checkpoint_rx.recv() => {
                if let Some(checkpoint) = checkpoint {
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

            _ = retry.tick(), if projection_attempt.is_none() && !finality_stalled => {
                if pending_target.is_some() {
                    can_start_attempt = true;
                } else {
                    match provider.finalized_block_num_hash() {
                        Ok(Some(block)) => {
                            let target = FinalizedTarget::new(block.number, block.hash);
                            match record_finalized_target(
                                &mut latest_target,
                                &mut pending_target,
                                target,
                            ) {
                                Ok(should_attempt) => can_start_attempt |= should_attempt,
                                Err(error) => {
                                    error!(%error, "rejected unsafe finalized projection target");
                                    can_start_attempt = false;
                                    finality_stalled = true;
                                }
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

fn project_through_target<P>(
    provider: P,
    runtime: &Mutex<ProjectionRuntime>,
    target: FinalizedTarget,
    durable_checkpoint_tx: &tokio::sync::mpsc::UnboundedSender<FinalizedTarget>,
) -> eyre::Result<Option<FinalizedTarget>>
where
    P: BlockReader,
{
    // Only one worker is launched at a time. The mutex also makes that ownership explicit and
    // keeps the mutable projector state available across retry attempts.
    let mut runtime = runtime
        .lock()
        .map_err(|_| eyre::eyre!("offchain-data projector lock is poisoned"))?;
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
            .ok_or_else(|| eyre::eyre!("canonical block {block_number} is unavailable"))?;
        let block = provider
            .block_by_hash(canonical_hash)
            .wrap_err_with(|| format!("load canonical block {block_number} ({canonical_hash})"))?
            .ok_or_else(|| {
                eyre::eyre!(
                    "canonical block {block_number} ({canonical_hash}) is unavailable by hash"
                )
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
            .ok_or_else(|| {
                eyre::eyre!("receipts for canonical block {block_number} are unavailable")
            })?;
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
        prepare_offchain_data_projection, project_through_target, record_finalized_target,
        run_projection_loop, FinalizedTarget, OffchainDataProjectionConfig, ProjectionRuntime,
    };
    use alloy_consensus::Header;
    use alloy_primitives::B256;
    use outbe_offchain_data::{OffchainDataProjection, ProjectionConfig};
    use outbe_offchain_storage::{
        AtomicWriteBatch, Key, MemoryStorage, Namespace, ScanPage, ScanRequest, StorageError,
        StorageReader, StorageWriter, StoredValue,
    };
    use reth_ethereum::{exex::ExExEvent, Block};
    use reth_provider::test_utils::MockEthProvider;

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
        let error = prepare_offchain_data_projection(OffchainDataProjectionConfig {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x11),
            start_block: 1,
            mongodb_uri: "mongodb://127.0.0.1:1/?directConnection=true&serverSelectionTimeoutMS=50"
                .to_owned(),
            mongodb_database: "startup_unavailable".to_owned(),
        })
        .err()
        .expect("unavailable MongoDB must fail startup preparation");

        assert!(error.to_string().contains("MongoDB"), "error: {error:#}");
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
    fn projects_each_intermediate_block_and_reports_each_durable_checkpoint() {
        let provider = MockEthProvider::new();
        let first = add_empty_block(&provider, 1);
        let second = add_empty_block(&provider, 2);
        let runtime = initialized_runtime(1);
        let (checkpoint_tx, mut checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();

        let result = project_through_target(
            provider,
            &runtime,
            FinalizedTarget::new(2, second),
            &checkpoint_tx,
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
        let provider = MockEthProvider::new();
        let first = add_empty_block(&provider, 1);
        let runtime = initialized_runtime(1);
        let (checkpoint_tx, mut checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();

        let error = project_through_target(
            provider,
            &runtime,
            FinalizedTarget::new(2, B256::repeat_byte(2)),
            &checkpoint_tx,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("canonical block 2 is unavailable"));
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
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            runtime,
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
    async fn post_startup_projection_failure_does_not_stop_evm_backed_node_loop() {
        use futures::{channel::mpsc, SinkExt};

        let provider = MockEthProvider::new();
        let block_hash = add_empty_block(&provider, 1);
        let storage = Arc::new(FailAfterStartupStorage::default());
        let projector = OffchainDataProjection::open(
            ProjectionConfig {
                chain_id: 1,
                genesis_hash: B256::repeat_byte(0x11),
                start_block: 1,
            },
            storage.clone(),
            storage.clone(),
        )
        .unwrap();
        storage.fail_writes.store(true, Ordering::SeqCst);

        let (mut notification_tx, notification_rx) = mpsc::channel(1);
        let (finality_tx, finality_rx) = mpsc::unbounded();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_projection_loop(
            provider,
            notification_rx,
            finality_rx,
            events_tx,
            ProjectionRuntime { projector },
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
        let projector = OffchainDataProjection::open(
            ProjectionConfig {
                chain_id: 1,
                genesis_hash: B256::repeat_byte(0x11),
                start_block,
            },
            storage.clone(),
            storage,
        )
        .unwrap();
        Mutex::new(ProjectionRuntime { projector })
    }

    #[derive(Debug, Default)]
    struct FailAfterStartupStorage {
        inner: MemoryStorage,
        fail_writes: AtomicBool,
        failed_writes: AtomicUsize,
    }

    impl StorageReader for FailAfterStartupStorage {
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

    impl StorageWriter for FailAfterStartupStorage {
        fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                self.failed_writes.fetch_add(1, Ordering::SeqCst);
                return Err(StorageError::Unavailable {
                    source: Box::new(std::io::Error::other(
                        "injected post-startup storage failure",
                    )),
                });
            }
            self.inner.apply_atomic(batch)
        }
    }
}
