use super::classify_retention_reconciliation;
use super::consume_projection_runtime_deadline;
use super::drain_exex_notifications;
use super::load_persisted_fatal_evidence;
use super::projection_runtime_failure;
use super::projection_runtime_watch_closed_failure;
use super::projection_task_failure;
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
use alloy_consensus::BlockHeader as _;

use alloy_primitives::B256;

use eyre::bail;
use eyre::Context as _;

use metrics::counter;
use metrics::gauge;
use metrics::histogram;

use outbe_node::finalized_frame::read_bounded_finalized_frames;

use outbe_node::finalized_frame::RethFinalizedFrameSource;
use outbe_node::ocomp::retention::observe_finalized_request;

use outbe_node::projection::FinalizedProjectionSink;
use outbe_node::projection::FinalizedTargetReconciliationV1;

use outbe_node::projection::ReadyOffchainDataProjection;

use outbe_node::projection::PROJECTION_RECOVERY_DEADLINE;

use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_ocomp::discovery_spool::DiscoverySpoolV1;
use outbe_ocomp::discovery_spool::RetirementReportV1;

use outbe_ocomp::embedded::EmbeddedOcompJobsV1;
use outbe_ocomp::embedded::EmbeddedOcompModeV1;

use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;
use outbe_ocomp::embedded_runtime::EmbeddedOcompBundleConfigV1;
use outbe_ocomp::embedded_runtime::EmbeddedOcompDomainConfigV1;
use outbe_ocomp::embedded_runtime::EmbeddedOcompDomainV1;

use outbe_ocomp_protocol::profile::poc_schema_limits;

use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionFailure;
use outbe_primitives::projection::ProjectionFailureClass;
use outbe_primitives::projection::ProjectionReadinessPublisher;
use outbe_primitives::projection::ProjectionStatus;

use outbe_primitives::OutbeReceipt;
use reth_ethereum::exex::ExExContext;

use reth_node_builder::FullNodeComponents;
use reth_primitives_traits::Block as _;
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
use std::time::Duration;

use tracing::warn;

pub(super) async fn wait_for_node_teardown() -> ! {
    std::future::pending::<()>().await;
    unreachable!("pending future completed")
}

pub async fn run_ocomp_exex<Node>(
    ctx: ExExContext<Node>,
    ready_projection: ReadyOffchainDataProjection,
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
    let domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
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
    .map_err(|error| eyre::eyre!("open Node-owned OCOMP domain: {error}"))?;
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
            .ok_or_else(|| eyre::eyre!("unified OCOMP closure checkpoint is unavailable"))?;
        if canonical != closed.block_hash {
            bail!("unified OCOMP closure checkpoint conflicts with canonical history");
        }
    }
    readiness.publish(ProjectionStatus::CatchingUp {
        checkpoint: Some(closed),
    });
    let projection_sink = Arc::new(std::sync::Mutex::new(FinalizedProjectionSink::new(
        ready_projection,
    )));
    let (mut projection_runtime_failures, projection_runtime_recovery) = {
        let sink = projection_sink
            .lock()
            .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?;
        (
            sink.runtime_failure_receiver()?,
            sink.runtime_recovery_handle()?,
        )
    };
    {
        let sink = projection_sink
            .lock()
            .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?;
        if let Some(projected) = sink.durable_checkpoint() {
            if projected.block_number < closed.block_number
                || (projected.block_number == closed.block_number
                    && projected.block_hash != closed.block_hash)
            {
                bail!("unified OCOMP closure checkpoint is ahead of durable projection");
            }
        } else if closed.block_number != 0 {
            bail!("unified OCOMP closure exists before the durable projection checkpoint");
        }
    }
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
    let (payout_tx, payout_rx) = mpsc::channel();
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
        payout_tx,
        payout_rx,
        payout_active: false,
        materialization_attempt_heights: BTreeMap::new(),
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        fatal: None,
    };

    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut projection_health = tokio::time::interval(Duration::from_millis(250));
    projection_health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut projection_unavailable_since = None;
    let mut projection_runtime_recovery_task = None;
    // Keep one sampled target throughout catch-up. Historical voting state
    // never authorizes effects; mandatory Completed verification uses this target.
    let mut recovery_target = None;
    let mut initial_finished_height_published = false;
    loop {
        let projection_runtime_deadline =
            projection_unavailable_since.map(|(_, since)| since + PROJECTION_RECOVERY_DEADLINE);
        tokio::select! {
            _ = wait_for_optional_deadline(projection_runtime_deadline) => {
                if let Some(failure) = consume_projection_runtime_deadline(
                    &projection_runtime_failures,
                    &mut projection_unavailable_since,
                ) {
                    projection_sink
                        .lock()
                        .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                        .publish_failure(failure.clone());
                    runtime.latch_external_failure(failure);
                    wait_for_node_teardown().await;
                }
            }
            _ = projection_health.tick() => {
                if let Some(failure) = projection_runtime_failure(
                    &projection_runtime_failures,
                    &mut projection_unavailable_since,
                    &projection_runtime_recovery,
                    &mut projection_runtime_recovery_task,
                ).await {
                    projection_sink
                        .lock()
                        .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                        .publish_failure(failure.clone());
                    runtime.latch_external_failure(failure);
                    wait_for_node_teardown().await;
                }
            }
            changed = projection_runtime_failures.changed() => {
                let failure = if changed.is_err() {
                    Some(projection_runtime_watch_closed_failure())
                } else {
                    projection_runtime_failure(
                        &projection_runtime_failures,
                        &mut projection_unavailable_since,
                        &projection_runtime_recovery,
                        &mut projection_runtime_recovery_task,
                    ).await
                };
                if let Some(failure) = failure {
                    projection_sink
                        .lock()
                        .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                        .publish_failure(failure.clone());
                    runtime.latch_external_failure(failure);
                    wait_for_node_teardown().await;
                }
            }
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
                while let Ok(outcome) = runtime.payout_rx.try_recv() {
                    runtime.handle_payout(outcome);
                }

                let finalized_target = runtime
                    .provider
                    .finalized_block_num_hash()
                    .wrap_err("sample unified finalized head")?;
                let reconciliation = projection_sink
                    .lock()
                    .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                    .reconcile_finalized_target(finalized_target.as_ref().map(|target| {
                        ProjectionCheckpoint {
                            block_number: target.number,
                            block_hash: target.hash,
                        }
                    }))?;
                let finalized_target = match reconciliation {
                    FinalizedTargetReconciliationV1::AwaitingProviderRecovery => None,
                    FinalizedTargetReconciliationV1::Process {
                        target,
                        recovered_floor,
                    } => {
                        let sampled = finalized_target.ok_or_else(|| {
                            eyre::eyre!("projection accepted an absent finalized target")
                        })?;
                        if sampled.number != target.block_number || sampled.hash != target.block_hash {
                            bail!("projection reconciled a different finalized target identity");
                        }
                        if let Some(floor) = recovered_floor {
                            let canonical = runtime
                                .provider
                                .block_hash(floor.block_number)
                                .wrap_err("revalidate recovered durable projection floor")?
                                .ok_or_else(|| {
                                    eyre::eyre!(
                                        "recovered durable projection floor {} ({}) is unavailable",
                                        floor.block_number,
                                        floor.block_hash
                                    )
                                })?;
                            if canonical != floor.block_hash {
                                bail!(
                                    "recovered durable projection floor {} changed from {} to {}",
                                    floor.block_number,
                                    floor.block_hash,
                                    canonical
                                );
                            }
                        }
                        Some(*recovery_target.get_or_insert(sampled))
                    }
                };
                if let Some(target) = finalized_target {
                    if !initial_finished_height_published {
                        let checkpoint = runtime.closure_checkpoint.current()?;
                        if checkpoint.block_number > target.number {
                            bail!("durable OCOMP closure is ahead of recovered finality");
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
                        .ok_or_else(|| eyre::eyre!("unified finalized height overflow"))?;
                    let source = frame_source.clone();
                    let batch = tokio::task::spawn_blocking(move || {
                        read_bounded_finalized_frames(&source, next_height, target)
                    })
                    .await
                    .wrap_err("unified finalized reader worker failed")??;
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
                            let sink = Arc::clone(&projection_sink);
                            let projected_frame = frame.clone();
                            let projection_deadline =
                                tokio::time::Instant::now() + PROJECTION_RECOVERY_DEADLINE;
                            let projection_write_deadline = projection_deadline.into_std();
                            let mut projection_task = tokio::task::spawn_blocking(move || {
                                sink.lock()
                                    .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                                    .project_frame_until(&projected_frame, projection_write_deadline)
                            });
                            let projection_result = loop {
                                let runtime_recovery_deadline = projection_unavailable_since
                                    .map(|(_, since)| since + PROJECTION_RECOVERY_DEADLINE);
                                tokio::select! {
                                    biased;
                                    _ = tokio::time::sleep_until(projection_deadline) => {
                                        let failure = ProjectionFailure::new(
                                            ProjectionFailureClass::MongoReconnectDeadline,
                                            "unified durable projection exceeded the recovery deadline",
                                        );
                                        runtime.latch_external_failure(failure);
                                        wait_for_node_teardown().await;
                                    }
                                    _ = wait_for_optional_deadline(runtime_recovery_deadline) => {
                                        if let Some(failure) = consume_projection_runtime_deadline(
                                            &projection_runtime_failures,
                                            &mut projection_unavailable_since,
                                        ) {
                                            runtime.latch_external_failure(failure);
                                            wait_for_node_teardown().await;
                                        }
                                    }
                                    result = &mut projection_task => break result,
                                    _ = projection_health.tick() => {
                                        if let Some(failure) = projection_runtime_failure(
                                            &projection_runtime_failures,
                                            &mut projection_unavailable_since,
                                            &projection_runtime_recovery,
                                            &mut projection_runtime_recovery_task,
                                        ).await {
                                            runtime.latch_external_failure(failure);
                                            wait_for_node_teardown().await;
                                        }
                                    }
                                    changed = projection_runtime_failures.changed() => {
                                        let failure = if changed.is_err() {
                                            Some(projection_runtime_watch_closed_failure())
                                        } else {
                                            projection_runtime_failure(
                                                &projection_runtime_failures,
                                                &mut projection_unavailable_since,
                                                &projection_runtime_recovery,
                                                &mut projection_runtime_recovery_task,
                                            ).await
                                        };
                                        if let Some(failure) = failure {
                                            runtime.latch_external_failure(failure);
                                            wait_for_node_teardown().await;
                                        }
                                    }
                                }
                            };
                            if let Err(error) = projection_result
                                .wrap_err("unified projection worker failed")
                                .and_then(|result| result)
                            {
                                let failure = projection_task_failure(error);
                                projection_sink
                                    .lock()
                                    .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                                    .publish_failure(failure.clone());
                                runtime.latch_external_failure(failure);
                                wait_for_node_teardown().await;
                            }
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
                                runtime.drive_payout(frame.block().header().timestamp());
                            }
                        }
                    }
                    projection_sink
                        .lock()
                        .map_err(|_| eyre::eyre!("unified projection sink lock is poisoned"))?
                        .publish_progress(ProjectionCheckpoint {
                            block_number: target.number,
                            block_hash: target.hash,
                        })?;
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
                    counter!("outbe_ocomp_finalized_loop_errors_total").increment(1);
                    runtime.latch_fatal(B256::ZERO, format!("unified finalized loop failed: {error:#}"))?;
                    wait_for_node_teardown().await;
                }
            }
        }
    }
}

async fn wait_for_optional_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
