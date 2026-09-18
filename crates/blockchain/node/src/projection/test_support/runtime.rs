#[cfg(test)]
use super::super::normalize_finalized_block;
use super::super::publish_status;
#[cfg(test)]
use super::super::readiness_checkpoint;
use super::super::DurableProjectionWrite;
use super::super::FinalizedTarget;
use super::super::ProjectionExit;
use super::super::ProjectionRuntime;
use super::super::PROJECTION_RECOVERY_DEADLINE;
use super::super::PROJECTION_RETRY_INTERVAL;
use super::admit_startup_finalized_target;
use super::projection_failure_class;
use super::projection_is_unavailable;
use super::publish_fatal;
use super::publish_progress;
use super::publish_projection_failure;
use super::record_or_publish_finalized_target;
use super::run_durable_projection_writer;
use super::FinalizedTargetDisposition;
#[cfg(test)]
use alloy_consensus::BlockHeader;
#[cfg(test)]
use alloy_primitives::Sealable;
use alloy_primitives::B256;
use eyre::bail;
use eyre::Context;
#[cfg(test)]
use futures::FutureExt;
#[cfg(test)]
use futures::Stream;
#[cfg(test)]
use futures::StreamExt;
use metrics::gauge;
use outbe_offchain_data::ProjectionFailureClass;
use outbe_offchain_data::ProjectionOutcome;
use outbe_offchain_data::ProjectionReadinessPublisher;
use outbe_offchain_data::ProjectionStatus;
use outbe_offchain_data::RuntimeBodyFailure;
use outbe_primitives::projection::ProjectionCheckpoint;
#[cfg(test)]
use reth_ethereum::exex::ExExEvent;
use reth_primitives_traits::Block;
use reth_provider::BlockIdReader;
#[cfg(test)]
use reth_provider::BlockReader;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use tokio::time::MissedTickBehavior;
#[cfg(test)]
use tracing::error;
use tracing::info;
use tracing::warn;

#[cfg(test)]
#[derive(Debug, thiserror::Error)]
pub(in super::super) enum HistoricalProjectionDataError {
    #[error("canonical block {block_number} is unavailable")]
    CanonicalBlock { block_number: u64 },
    #[error("canonical block {block_number} ({block_hash}) is unavailable by hash")]
    CanonicalBlockByHash { block_number: u64, block_hash: B256 },
    #[error("receipts for canonical block {block_number} are unavailable")]
    Receipts { block_number: u64 },
}

#[cfg(test)]
type ProjectionAttempt = tokio::sync::oneshot::Receiver<eyre::Result<Option<FinalizedTarget>>>;

#[cfg(test)]
pub(in super::super) fn spawn_detached_projection_work<T: Send + 'static>(
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

#[cfg(test)]
pub(in super::super) async fn supervise_projection_future<F>(
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

#[cfg(test)]
pub(in super::super) async fn run_projection_loop<P, N, F>(
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
    let durable_writer = runtime.writer.clone();
    let mut runtime_failures = runtime
        .runtime_failure_receiver
        .take()
        .ok_or_else(|| eyre::eyre!("projection body-read failure receiver is unavailable"))?;
    let projector = Arc::new(Mutex::new(runtime));
    let (logical_checkpoint_tx, mut logical_checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();
    let (durable_checkpoint_tx, mut durable_checkpoint_rx) = tokio::sync::mpsc::unbounded_channel();
    let (durable_write_tx, durable_write_rx) = tokio::sync::mpsc::unbounded_channel();
    let (recovery_ack_tx, mut recovery_ack_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("offchain-storage-writer".to_owned())
        .spawn(move || {
            run_durable_projection_writer(durable_writer, durable_write_rx, durable_checkpoint_tx);
        })
        .wrap_err("spawn offchain-data offchain storage writer")?;

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
    let mut storage_unavailable_since: Option<tokio::time::Instant> = None;
    let mut immediate_recovery_used = false;

    let retry_start = tokio::time::Instant::now() + PROJECTION_RETRY_INTERVAL;
    let mut retry = tokio::time::interval_at(retry_start, PROJECTION_RETRY_INTERVAL);
    retry.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        if projection_attempt.is_none() && can_start_attempt && !finality_stalled {
            if let Some(target) = pending_target {
                let provider = provider.clone();
                let projector = Arc::clone(&projector);
                let logical_checkpoint_tx = logical_checkpoint_tx.clone();
                let durable_write_tx = durable_write_tx.clone();
                let recovery_ack_tx = recovery_ack_tx.clone();
                match spawn_detached_projection_work("offchain-projector", move || {
                    project_through_target(
                        provider,
                        &projector,
                        target,
                        &logical_checkpoint_tx,
                        &durable_write_tx,
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
                        match record_or_publish_finalized_target(
                            &mut latest_target,
                            &mut pending_target,
                            target,
                            &readiness_publisher,
                            &projection_exit,
                        ) {
                            FinalizedTargetDisposition::Attempt => can_start_attempt = true,
                            FinalizedTargetDisposition::Unchanged => can_start_attempt = false,
                            FinalizedTargetDisposition::Rejected => {
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

            changed = runtime_failures.changed(), if runtime_failures_open && !finality_stalled => {
                match changed {
                    Ok(()) => match runtime_failures.borrow_and_update().clone() {
                    Some(RuntimeBodyFailure::Unavailable { .. }) => {
                        let since = *storage_unavailable_since
                            .get_or_insert_with(tokio::time::Instant::now);
                        publish_status(
                            &readiness_publisher,
                            ProjectionStatus::MongoUnavailable {
                                checkpoint: readiness_checkpoint(&readiness_publisher.current()),
                                since: since.into_std(),
                            },
                            latest_target,
                        );
                        gauge!("outbe_projection_storage_reconnect_active").set(1.0);
                        gauge!("outbe_projection_storage_reconnect_remaining_seconds")
                            .set(PROJECTION_RECOVERY_DEADLINE.as_secs_f64());
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
                        storage_unavailable_since = None;
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
                            let since = *storage_unavailable_since
                                .get_or_insert_with(tokio::time::Instant::now);
                            publish_status(
                                &readiness_publisher,
                                ProjectionStatus::MongoUnavailable {
                                    checkpoint: readiness_checkpoint(&readiness_publisher.current()),
                                    since: since.into_std(),
                                },
                                latest_target,
                            );
                            gauge!("outbe_projection_storage_reconnect_active").set(1.0);
                            gauge!("outbe_projection_storage_reconnect_remaining_seconds").set(
                                PROJECTION_RECOVERY_DEADLINE
                                    .saturating_sub(since.elapsed())
                                    .as_secs_f64(),
                            );
                            if since.elapsed() >= PROJECTION_RECOVERY_DEADLINE {
                                publish_fatal(
                                    &readiness_publisher,
                                    &projection_exit,
                                    ProjectionFailureClass::MongoReconnectDeadline,
                                    "offchain storage reconnect deadline expired",
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

            checkpoint = logical_checkpoint_rx.recv(), if !finality_stalled => {
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
                }
            }

            checkpoint = durable_checkpoint_rx.recv(), if !finality_stalled => {
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

            recovered = recovery_ack_rx.recv(), if !finality_stalled => {
                if recovered.is_some() && storage_unavailable_since.take().is_some() {
                    immediate_recovery_used = false;
                    publish_status(
                        &readiness_publisher,
                        ProjectionStatus::CatchingUp {
                            checkpoint: readiness_checkpoint(&readiness_publisher.current()),
                        },
                        latest_target,
                    );
                    gauge!("outbe_projection_storage_reconnect_active").set(0.0);
                    gauge!("outbe_projection_storage_reconnect_remaining_seconds").set(0.0);
                }
            }

            _ = async {
                match storage_unavailable_since {
                    Some(since) => {
                        tokio::time::sleep_until(since + PROJECTION_RECOVERY_DEADLINE).await
                    }
                    None => std::future::pending().await,
                }
            }, if !finality_stalled => {
                publish_fatal(
                    &readiness_publisher,
                    &projection_exit,
                    ProjectionFailureClass::MongoReconnectDeadline,
                    "offchain storage reconnect deadline expired",
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
                            match record_or_publish_finalized_target(
                                &mut latest_target,
                                &mut pending_target,
                                target,
                                &readiness_publisher,
                                &projection_exit,
                            ) {
                                FinalizedTargetDisposition::Attempt => can_start_attempt = true,
                                FinalizedTargetDisposition::Unchanged => can_start_attempt = false,
                                FinalizedTargetDisposition::Rejected => {
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

#[cfg(test)]
pub(in super::super) fn project_through_target<P>(
    provider: P,
    runtime: &Mutex<ProjectionRuntime>,
    target: FinalizedTarget,
    logical_checkpoint_tx: &tokio::sync::mpsc::UnboundedSender<FinalizedTarget>,
    durable_write_tx: &tokio::sync::mpsc::UnboundedSender<DurableProjectionWrite>,
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
    if runtime
        .runtime_failure_sender
        .as_ref()
        .is_some_and(|sender| {
            matches!(
                *sender.borrow(),
                Some(RuntimeBodyFailure::Unavailable { .. })
            )
        })
    {
        runtime
            .writer
            .verify_transaction_capability()
            .wrap_err("probe offchain storage before acknowledging runtime-body recovery")?;
    }
    let overlay = runtime.overlay.clone();
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
        Some(checkpoint) if checkpoint.block_number > target.number => {
            return Err(eyre::eyre!(
                "projection checkpoint {} ({}) is ahead of finalized target {} ({})",
                checkpoint.block_number,
                checkpoint.block_hash,
                target.number,
                target.hash
            ))
        }
        Some(checkpoint)
            if checkpoint.block_number == target.number && checkpoint.block_hash != target.hash =>
        {
            return Err(eyre::eyre!(
                "projection checkpoint hash {} conflicts with finalized hash {} at height {}",
                checkpoint.block_hash,
                target.hash,
                target.number
            ));
        }
        Some(checkpoint) if checkpoint.block_number == target.number => {
            let checkpoint = FinalizedTarget::new(checkpoint.block_number, checkpoint.block_hash);
            logical_checkpoint_tx
                .send(checkpoint)
                .map_err(|_| eyre::eyre!("logical checkpoint receiver is closed"))?;
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
        let normalized = normalize_finalized_block(block_number, block_hash, &block, &receipts)?;

        let prepared = projector
            .prepare_block(&normalized)
            .wrap_err_with(|| format!("project finalized block {block_number}"))?;
        let (projected, durable_batch) = projector
            .apply_prepared_with_batch(prepared)
            .wrap_err_with(|| format!("apply logical finalized block {block_number}"))?;
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
        let overlay_ack = overlay
            .as_ref()
            .map(|overlay| (Arc::clone(overlay), overlay.current_generation()));
        durable_write_tx
            .send(DurableProjectionWrite {
                checkpoint: FinalizedTarget::new(projected.block_number, projected.block_hash),
                batch: durable_batch,
                overlay_ack,
            })
            .map_err(|_| eyre::eyre!("durable offchain storage writer queue is closed"))?;
        logical_checkpoint_tx
            .send(FinalizedTarget::new(
                projected.block_number,
                projected.block_hash,
            ))
            .map_err(|_| eyre::eyre!("logical checkpoint receiver is closed"))?;
    }

    Ok(durable_checkpoint)
}
