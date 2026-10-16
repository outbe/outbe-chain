use super::classify_retention_reconciliation;
use super::drain_exex_notifications;
use super::finished_height_consumer_closed_during_shutdown;
use super::load_persisted_fatal_evidence;
use super::publish_finalized_reader_metrics;
use super::publish_finished_height;
use super::record_discovery_retirement_report;
use super::retention_runtime_error_requires_frame_retry;
use super::without_execution_backfill;
use super::EmbeddedOcompExExV1;
use super::OcompExExConfigV1;
use super::OcompExExExitV1;
use super::OcompReadinessV1;
use super::RetentionReconciliationDispositionV1;
use super::POLL_INTERVAL;
use crate::errors::OcompExExError;
use alloy_primitives::B256;

use eyre::Context as _;

use metrics::counter;
use metrics::gauge;
use metrics::histogram;

use outbe_node::finalized_frame::read_bounded_finalized_frames;

use outbe_node::finalized_frame::RethFinalizedFrameSource;
use outbe_node::ocomp::retention::observe_finalized_request;

use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_ocomp::discovery_spool::DiscoverySpoolV1;
use outbe_ocomp::discovery_spool::RetirementReportV1;

use outbe_ocomp::embedded::EmbeddedOcompJobsV1;
use outbe_ocomp::embedded::EmbeddedOcompModeV1;

use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;
use outbe_ocomp::embedded_runtime::EmbeddedOcompBundleConfigV1;
use outbe_ocomp::embedded_runtime::EmbeddedOcompDomainConfigV1;

use outbe_ocomp_protocol::profile::poc_schema_limits;

use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionFailure;
use outbe_primitives::projection::ProjectionFailureClass;
use outbe_primitives::projection::ProjectionReadinessPublisher;
use outbe_primitives::projection::ProjectionStatus;

use outbe_primitives::OutbeReceipt;
use reth_ethereum::exex::ExExContext;

use reth_node_builder::FullNodeComponents;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;
use reth_provider::BlockNumReader;
use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use std::sync::mpsc;
use std::sync::Arc;

use tracing::warn;

pub(super) async fn wait_for_node_teardown() -> ! {
    std::future::pending::<()>().await;
    unreachable!("pending future completed")
}

pub async fn run_ocomp_exex<Node>(
    ctx: ExExContext<Node>,
    config: OcompExExConfigV1,
    readiness: ProjectionReadinessPublisher,
    exit: tokio::sync::mpsc::UnboundedSender<OcompExExExitV1>,
) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: BlockIdReader
        + BlockHashReader
        + BlockNumReader
        + BlockReader
        + ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    gauge!("outbe_ocomp_finalized_loop_fatal").set(0.0);
    let shutdown = ctx.task_executor().on_shutdown_signal().clone();
    let readiness = OcompReadinessV1(Arc::new(std::sync::Mutex::new(readiness)));
    let provider = ctx.provider().clone();
    // OCOMP owns a nonexecuting finalized reader. Its closure is not an EVM
    // execution head, and must never select Reth historical execution backfill.
    let notifications = without_execution_backfill(ctx.notifications);
    let drain_readiness = readiness.clone();
    let drain_exit = exit.clone();
    let mut notification_drain = tokio::task::JoinSet::new();
    notification_drain.spawn(async move {
        let error = drain_exex_notifications(notifications).await;
        let failure = ProjectionFailure::new(ProjectionFailureClass::Other, error.to_string());
        drain_readiness.publish(ProjectionStatus::Fatal {
            checkpoint: None,
            error: failure.clone(),
        });
        let _ = drain_exit.send(OcompExExExitV1 { failure });
        error
    });
    let limits = poc_schema_limits();
    let mut discovery_spools = BTreeMap::new();
    for bundle in &config.bundles {
        let bundle_hash = bundle.protocol_bundle.hash();
        discovery_spools.insert(
            bundle_hash,
            DiscoverySpoolV1::open(
                config
                    .discovery_spool_root
                    .join(hex::encode(bundle_hash.as_slice())),
                config.chain_id,
                config.genesis_hash,
                limits,
            )?,
        );
    }
    let closure_checkpoint = ContiguousCheckpointStoreV1::open(
        config.discovery_spool_root.join("closure-checkpoint-v1"),
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: config.genesis_hash,
        },
    )?;
    let domain = outbe_ocomp::embedded_runtime::open_embedded_domain(EmbeddedOcompDomainConfigV1 {
        domain_root: config.domain_root,
        registry_generation: 1,
        bundles: config
            .bundles
            .into_iter()
            .map(|bundle| EmbeddedOcompBundleConfigV1 {
                worker_address: bundle.worker_address,
                identity: bundle.identity,
                protocol_bundle: bundle.protocol_bundle,
            })
            .collect(),
        policy: config.policy,
        validator_rpc_url: config.validator_rpc_url,
        limits,
    })
    .map_err(|error| OcompExExError::OpenNodeOwnedDomain(crate::errors::display_error(error)))?;
    if let Some(detail) = load_persisted_fatal_evidence(domain.fatal_evidence_root())? {
        gauge!("outbe_ocomp_finalized_loop_fatal").set(1.0);
        let failure = ProjectionFailure::new(
            ProjectionFailureClass::Other,
            format!("embedded OCOMP persisted fatal evidence: {detail}"),
        );
        readiness.publish(ProjectionStatus::Fatal {
            checkpoint: None,
            error: failure.clone(),
        });
        let _ = exit.send(OcompExExExitV1 { failure });
        wait_for_node_teardown().await;
    }
    let closed = closure_checkpoint.current()?;
    if closed.block_number > 0 {
        let canonical = provider
            .block_hash(closed.block_number)
            .wrap_err("validate unified OCOMP closure checkpoint")?
            .ok_or(OcompExExError::UnifiedClosureCheckpointUnavailable)?;
        if canonical != closed.block_hash {
            return Err(OcompExExError::UnifiedClosureCheckpointConflictsWithHistory.into());
        }
    }
    readiness.publish(ProjectionStatus::CatchingUp {
        checkpoint: Some(closed),
    });
    let mut startup_retirements = RetirementReportV1::default();
    for spool in discovery_spools.values() {
        let report = spool.complete_retirements_through(closed.block_number)?;
        startup_retirements.completed = startup_retirements
            .completed
            .saturating_add(report.completed);
        startup_retirements.waiting_for_checkpoint = startup_retirements
            .waiting_for_checkpoint
            .saturating_add(report.waiting_for_checkpoint);
    }
    record_discovery_retirement_report(startup_retirements);
    let frame_source = RethFinalizedFrameSource::new(provider.clone());
    let mode = match config.policy {
        EmbeddedNodePolicyV1::Validator => EmbeddedOcompModeV1::Validator,
        EmbeddedNodePolicyV1::FullNode => EmbeddedOcompModeV1::FullNode,
    };
    let (compute_tx, compute_rx) = mpsc::channel();
    let (vote_tx, vote_rx) = mpsc::channel();
    let (materialization_tx, materialization_rx) = mpsc::channel();
    let mut runtime = EmbeddedOcompExExV1 {
        provider,
        policy: config.policy,
        domain,
        readiness,
        exit,
        requests: BTreeMap::new(),
        materialized_requests: BTreeSet::new(),
        jobs: BTreeMap::new(),
        intent_jobs: BTreeMap::new(),
        discovery_spools,
        pending_offers: BTreeMap::new(),
        acknowledged_exports: BTreeSet::new(),
        retention_selector: config.retention_selector,
        closure_checkpoint,
        latest_scanned_checkpoint: closed,
        state: EmbeddedOcompJobsV1::new(mode),
        scanned_height: closed.block_number,
        scanned_hash: closed.block_hash,
        compute_tx,
        compute_rx,
        vote_tx,
        vote_rx,
        materialization_tx,
        materialization_rx,
        materialization_active: None,
        materialization_attempt_heights: BTreeMap::new(),
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        fatal: None,
    };

    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut provider_recovery_floor = Some(closed);
    // Keep one sampled target throughout catch-up. Historical voting state
    // never authorizes effects. Mandatory Completed verification uses this target.
    let mut recovery_target = None;
    let mut initial_finished_height_published = false;
    loop {
        tokio::select! {
            result = notification_drain.join_next() => {
                runtime.latch_external_failure(ProjectionFailure::new(
                    ProjectionFailureClass::Other,
                    format!("OCOMP notification drain terminated: {result:?}"),
                ));
                wait_for_node_teardown().await;
            }
            _ = poll.tick() => {
                let tick_result: eyre::Result<()> = async {
                while let Ok(outcome) = runtime.compute_rx.try_recv() {
                    if let Err(error) = runtime.handle_compute(outcome) {
                        runtime.latch_fatal(B256::ZERO, format!("{error:#}"))?;
                        wait_for_node_teardown().await;
                    }
                    if runtime.fatal.is_some() {
                        wait_for_node_teardown().await;
                    }
                }
                while let Ok(outcome) = runtime.vote_rx.try_recv() {
                    if let Err(error) = runtime.handle_vote(outcome) {
                        runtime.latch_fatal(B256::ZERO, format!("{error:#}"))?;
                        wait_for_node_teardown().await;
                    }
                    if runtime.fatal.is_some() {
                        wait_for_node_teardown().await;
                    }
                }
                while let Ok(outcome) = runtime.materialization_rx.try_recv() {
                    runtime.handle_materialization(outcome);
                }

                let finalized_target = runtime
                    .provider
                    .finalized_block_num_hash()
                    .wrap_err("sample unified finalized head")?;
                // Restore the existing durable OCOMP closure floor before processing
                // finalized history. Native state replaces the deleted CE projection.
                let floor = runtime.closure_checkpoint.current()?;
                let finalized_target = match finalized_target {
                    None => { provider_recovery_floor = Some(floor); None },
                    Some(target) if target.number < floor.block_number => {
                        provider_recovery_floor = Some(floor);
                        None
                    },
                    Some(target) => {
                        if target.number == floor.block_number && target.hash != floor.block_hash {
                            return Err(OcompExExError::RecoveredClosureFloorChanged {
                                block_number:floor.block_number,expected:floor.block_hash,actual:target.hash,
                            }.into());
                        }
                        if let Some(recovered) = provider_recovery_floor.take() {
                            let canonical = runtime.provider.block_hash(recovered.block_number)?
                                .ok_or(OcompExExError::RecoveredClosureFloorUnavailable {
                                    block_number:recovered.block_number,block_hash:recovered.block_hash,
                                })?;
                            if canonical != recovered.block_hash {
                                return Err(OcompExExError::RecoveredClosureFloorChanged {
                                    block_number:recovered.block_number,expected:recovered.block_hash,actual:canonical,
                                }.into());
                            }
                        }
                        Some(*recovery_target.get_or_insert(target))
                    }
                };
                if let Some(target) = finalized_target {
                    if !initial_finished_height_published {
                        let checkpoint = runtime.closure_checkpoint.current()?;
                        if checkpoint.block_number > target.number {
                            return Err(OcompExExError::DurableClosureAheadOfRecoveredFinality.into());
                        }
                        publish_finished_height(&ctx.events, checkpoint)?;
                        initial_finished_height_published = true;
                    }
                    publish_finalized_reader_metrics(
                        target.number,
                        runtime.scanned_height,
                        runtime.closure_checkpoint.current()?.block_number,
                    );
                    let next_height = runtime
                        .scanned_height
                        .checked_add(1)
                        .ok_or(OcompExExError::UnifiedFinalizedHeightOverflow)?;
                    let source = frame_source.clone();
                    let batch = match tokio::task::spawn_blocking(move || {
                        read_bounded_finalized_frames(&source, next_height, target)
                    })
                    .await
                    .wrap_err("unified finalized reader worker failed")?
                    {
                        Ok(batch) => batch,
                        Err(error)
                            if error
                                .downcast_ref::<outbe_node::errors::FinalizedFrameError>()
                                .is_some_and(
                                    outbe_node::errors::FinalizedFrameError::is_transient_unavailability,
                                ) =>
                        {
                            warn!(
                                %error,
                                next_height,
                                target_height = target.number,
                                "unified finalized frame temporarily unavailable from provider; retrying on next poll"
                            );
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    if let Some(batch) = batch {
                        histogram!("outbe_ocomp_finalized_reader_batch_blocks")
                            .record(batch.frames().len() as f64);
                        'frames: for frame in batch.frames() {
                            let request_observation = observe_finalized_request(frame)?;
                            match classify_retention_reconciliation(
                                runtime
                                    .retention_selector
                                    .reconcile_finalized_frame(frame, request_observation),
                                config.retention_required,
                            ) {
                                RetentionReconciliationDispositionV1::ProcessFrame => {}
                                RetentionReconciliationDispositionV1::Fatal(error) => {
                                    return Err(error).wrap_err("finalized retention identity conflict");
                                }
                                RetentionReconciliationDispositionV1::RetryFrame(error) => {
                                    warn!(
                                        %error,
                                        block_number = frame.identity().number,
                                        block_hash = %frame.identity().hash,
                                        "unified OCOMP retention is not ready; retrying the same finalized frame"
                                    );
                                    break 'frames;
                                }
                            }
                            runtime.record_request_observation(frame, request_observation)?;
                            runtime.record_scanned_frame(frame)?;
                            // Reconcile against the sampled target as history arrives.
                            // Suppress old voting/offer effects, but preserve mandatory
                            // FullNode verification of canonically Completed jobs.
                            if let Err(error) = runtime.refresh_jobs(target.number, target.hash, false).await {
                                if retention_runtime_error_requires_frame_retry(&error) {
                                    warn!(%error, "OCOMP replay awaiting durable retention metadata");
                                    break 'frames;
                                }
                                return Err(error);
                            }
                            counter!("outbe_ocomp_finalized_reader_blocks_total").increment(1);
                            if frame.identity() == target {
                                runtime.reconcile_materialization(frame)?;
                            }
                        }
                    }
                    runtime
                        .retention_selector
                        .notify_finalized_height(target.number);
                    if runtime.scanned_height == target.number {
                        if let Err(error) = runtime.refresh_jobs(target.number, target.hash, true).await {
                            if retention_runtime_error_requires_frame_retry(&error) {
                                warn!(%error, "OCOMP recovery awaiting durable retention; retrying finalized reconciliation");
                                return Ok(());
                            }
                            return Err(error);
                        }
                        recovery_target = None;
                    }
                }

                gauge!("outbe_ocomp_discovery_pending_offers")
                    .set(runtime.pending_offers.len() as f64);

                if let Some(checkpoint) = runtime.flush_closure_checkpoint()? {
                    publish_finished_height(&ctx.events, checkpoint)?;
                }
                if let Some(target) = finalized_target {
                    if recovery_target.is_none() {
                        runtime.publish_observation_progress(ProjectionCheckpoint {
                            block_number: target.number,
                            block_hash: target.hash,
                        })?;
                    }
                    publish_finalized_reader_metrics(
                        target.number,
                        runtime.scanned_height,
                        runtime.closure_checkpoint.current()?.block_number,
                    );
                }
                Ok(())
                }.await;
                if let Err(error) = tick_result {
                    if finished_height_consumer_closed_during_shutdown(&error, &shutdown) {
                        wait_for_node_teardown().await;
                    }
                    counter!("outbe_ocomp_finalized_loop_errors_total").increment(1);
                    runtime.latch_fatal(B256::ZERO, format!("unified finalized loop failed: {error:#}"))?;
                    wait_for_node_teardown().await;
                }
            }
        }
    }
}
