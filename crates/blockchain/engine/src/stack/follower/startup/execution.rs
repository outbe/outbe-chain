use super::super::super::*;
use super::super::{
    follower_height_has_certified_finalization, spawn_finalization_drainer,
    FollowerProofPersistence, ValidatorBootstrap,
};
use super::{FollowerRuntime, RecoveredFollowerHistory, StartupChainState};
use crate::stack::recovery::anchor::CertifiedFollowerRecoveryAnchor;
use crate::stack::recovery::forkchoice::read_reth_recovery_forkchoice;
use outbe_consensus::follow::{run_follow_engine, FollowEngineConfig, TipSource as _};
use outbe_consensus::marshal_types::MarshalMailbox;
use services::FollowerStackServices;

pub(super) struct ExecutionBootstrap<E: FollowerRuntime> {
    pub(super) ctx: E,
    pub(super) node: OutbeFullNode,
    pub(super) bridge: ConsensusExecutionBridge,
    pub(super) history: RecoveredFollowerHistory<E>,
    pub(super) startup: StartupChainState,
    pub(super) services: FollowerStackServices,
}
struct ConfirmedExecutor<E: FollowerRuntime> {
    actor: ExecutorActor<E>,
    mailbox: outbe_consensus::executor::Mailbox,
    finalized_heights: tokio::sync::mpsc::UnboundedReceiver<u64>,
}
struct ParentCertificateBootstrap<'a> {
    protocol: &'a dyn crate::protocol_state::EngineProtocolState,
    node: &'a OutbeFullNode,
    store: outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    recovery_anchor: &'a CertifiedFollowerRecoveryAnchor,
    certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
}
struct RecoveredExecutionBootstrap<'a> {
    bridge: &'a ConsensusExecutionBridge,
    recovery_anchor: &'a CertifiedFollowerRecoveryAnchor,
    last_consensus_finalized: Height,
    last_execution_height: u64,
}
impl<E: FollowerRuntime> ExecutionBootstrap<E> {
    async fn confirmed_executor(&self) -> Result<ConfirmedExecutor<E>> {
        let ctx = &self.ctx;
        let node = &self.node;
        let genesis_hash = self.startup.genesis_hash;
        let recovery_anchor = &self.history.recovery_anchor;
        let engine_handle: EngineHandle = node.add_ons_handle.beacon_engine_handle.clone();
        let (execution_finalized_height_tx, execution_finalized_height_rx) =
            tokio::sync::mpsc::unbounded_channel::<u64>();
        let (executor_actor, executor_mailbox) = ExecutorActor::new(
            ctx.child("executor"),
            engine_handle,
            outbe_consensus::executor::actor::RecoveredFinalizedState {
                genesis_hash,
                last_finalized_height: recovery_anchor.checkpoint.block_number,
                last_finalized_hash: recovery_anchor.checkpoint.block_hash,
            },
            Some(execution_finalized_height_tx),
        );
        let fcu_provider_node = node.clone();
        confirm_recovered_forkchoice(
            ctx.child("recovered_forkchoice"),
            recovery_anchor.checkpoint,
            || executor_actor.replay_recovered_forkchoice_once(recovery_anchor.checkpoint),
            move || {
                read_reth_recovery_forkchoice(
                    &fcu_provider_node.provider.canonical_in_memory_state(),
                )
            },
        )
        .await
        .wrap_err("failed to confirm recovered Reth forkchoice before follower startup")?;

        Ok(ConfirmedExecutor {
            actor: executor_actor,
            mailbox: executor_mailbox,
            finalized_heights: execution_finalized_height_rx,
        })
    }
    pub(super) async fn run(self) -> Result<crate::follower_shutdown::FollowStackExit> {
        let ConfirmedExecutor {
            actor: executor_actor,
            mailbox: executor_mailbox,
            finalized_heights: execution_finalized_height_rx,
        } = self.confirmed_executor().await?;
        let FollowerStackServices {
            finalized_execution_barrier,
            follower_shutdown,
            parent_cert_store,
            bootstrap,
            finalization_notify,
            protocol,
        } = self.services;
        let recovery_anchor = &self.history.recovery_anchor;
        finalized_execution_barrier
            .verify_checkpoint(recovery_anchor.checkpoint)
            .wrap_err("native execution is not durable at the certified follower anchor")?;
        let finalized_parent_cert_store = ParentCertificateBootstrap {
            protocol: protocol.as_ref(),
            node: &self.node,
            store: parent_cert_store,
            recovery_anchor,
            certificate_scheme_provider: &self.history.certificate_scheme_provider,
        }
        .install()?;
        RecoveredExecutionBootstrap {
            bridge: &self.bridge,
            recovery_anchor,
            last_consensus_finalized: self.history.last_consensus_finalized,
            last_execution_height: self.startup.last_execution_height,
        }
        .publish();
        let marshal_mailbox = &self.history.marshal_mailbox;
        let last_consensus_finalized = self.history.last_consensus_finalized;
        let executor_actor =
            executor_actor.with_finalized_execution_barrier(finalized_execution_barrier);
        let Some(executor_reporter) = follower_shutdown.install(executor_mailbox)? else {
            // Shutdown won the startup race. No execution delivery was accepted.
            return Ok(crate::follower_shutdown::FollowStackExit::Shutdown);
        };
        let observer_ingress = executor_reporter.clone();
        let executor_handle =
            executor_actor.start(marshal_mailbox.clone(), last_consensus_finalized);

        LiveFollower {
            ctx: self.ctx,
            node: self.node,
            bridge: self.bridge,
            history: self.history,
            finalized_parent_cert_store,
            executor_reporter,
            observer_ingress,
            executor_handle,
            execution_finalized_height_rx,
            follower_shutdown,
            bootstrap,
            finalization_notify,
            protocol,
        }
        .run()
        .await
    }
}
impl ParentCertificateBootstrap<'_> {
    fn install(
        self,
    ) -> Result<outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore> {
        let Self {
            protocol,
            node,
            store: finalized_parent_cert_store,
            recovery_anchor,
            certificate_scheme_provider,
        } = self;
        finalized_parent_cert_store
            .prune_above_height(recovery_anchor.checkpoint.block_number)
            .wrap_err("failed to prune follower parent certificates above recovery anchor")?;
        if let Some(finalization) = recovery_anchor.finalization.as_ref() {
            FollowerProofPersistence {
                protocol,
                node,
                certificate_scheme_provider,
                parent_cert_store: &finalized_parent_cert_store,
            }
            .reconcile_record(finalization, &recovery_anchor.block)
            .wrap_err("failed to reconcile recovered certified follower parent")?;
        }
        Ok(finalized_parent_cert_store)
    }
}
impl RecoveredExecutionBootstrap<'_> {
    fn publish(self) {
        let Self {
            bridge,
            recovery_anchor,
            last_consensus_finalized,
            last_execution_height,
        } = self;
        bridge.set_last_finalized_block_number(recovery_anchor.checkpoint.block_number);

        info!(
            marshal_processed = last_consensus_finalized.get(),
            recovery_height = recovery_anchor.checkpoint.block_number,
            recovery_hash = %recovery_anchor.checkpoint.block_hash,
            last_execution_height,
            "certified follower startup recovery barrier completed"
        );
    }
}
struct LiveFollower<E: FollowerRuntime> {
    ctx: E,
    node: OutbeFullNode,
    bridge: ConsensusExecutionBridge,
    history: RecoveredFollowerHistory<E>,
    finalized_parent_cert_store:
        outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    executor_reporter: crate::follower_shutdown::FollowerReporter,
    observer_ingress: crate::follower_shutdown::FollowerReporter,
    executor_handle: commonware_runtime::Handle<Result<()>>,
    execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
    follower_shutdown: crate::follower_shutdown::FollowerDrain,
    bootstrap: Option<ValidatorBootstrap>,
    finalization_notify: Arc<tokio::sync::Notify>,
    protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
}
impl<E: FollowerRuntime> LiveFollower<E> {
    async fn run(self) -> Result<crate::follower_shutdown::FollowStackExit> {
        let Self {
            ctx,
            node,
            bridge,
            history,
            finalized_parent_cert_store,
            executor_reporter,
            observer_ingress,
            executor_handle,
            execution_finalized_height_rx,
            follower_shutdown,
            bootstrap,
            finalization_notify,
            protocol,
        } = self;
        let RecoveredFollowerHistory {
            marshal_actor,
            marshal_mailbox,
            last_consensus_finalized,
            certificate_scheme_provider,
            chain,
            anchor_epoch,
            epocher,
            upstream_client,
            tip_client,
            local,
            ..
        } = history;
        // -- 4b. Serve `outbe_getFinalization`. The critical observer below owns
        // finality publication only after exact parent-proof persistence succeeds. --------------------------------------
        let (follower_phase_cancel, phase_cancel_rx) = tokio::sync::watch::channel(false);
        let (driver_cancel, driver_cancel_rx) = tokio::sync::watch::channel(false);
        let initial_upstream_tip = if bootstrap.is_some() {
            tip_client
                .finalized_tip()
                .await
                .ok_or(BootstrapPromotionError::UpstreamTipUnavailable)?
                .get()
        } else {
            0
        };
        let finalization_drainer = spawn_finalization_drainer(
            &ctx,
            marshal_mailbox.clone(),
            bridge.clone(),
            finalized_parent_cert_store.clone(),
            phase_cancel_rx.clone(),
        );

        // -- 5. Assemble + run the follower engine ----------------------------
        let finality_observer = FinalityObserver {
            observer_mailbox: marshal_mailbox.clone(),
            observer_node: node.clone(),
            observer_schemes: certificate_scheme_provider.clone(),
            observer_store: finalized_parent_cert_store.clone(),
            observer_bridge: bridge.clone(),
            observer_ingress,
            execution_finalized_height_rx,
            bootstrap,
            initial_upstream_tip,
            initial_persisted_height: last_consensus_finalized.get(),
            driver_cancel,
            finalization_notify,
            protocol,
        }
        .run();
        let follow_engine = run_follow_engine(
            ctx.child("follow_engine"),
            FollowEngineConfig {
                marshal_actor,
                marshal_mailbox,
                recovered_height: last_consensus_finalized,
                executor_reporter,
                upstream: upstream_client,
                local,
                tip: tip_client,
                epocher,
                chain,
                anchor_epoch,
                mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
                driver_cancel: driver_cancel_rx,
                follower_phase_cancel: phase_cancel_rx,
            },
        );
        let executor_exit = async move {
            executor_handle.await.map_err(|error| {
                crate::errors::FollowerStackError::ExecutorTaskFailed(format!("{error:?}"))
            })?
        };
        // A clean marshal stop must not hide an executor error observed while its
        // mailbox drains, nor discard a queued finality reconciliation.
        let accepted_work = async move {
            follower_shutdown
                .await_work(executor_exit, finality_observer)
                .await?;
            follower_phase_cancel.send_replace(true);
            finalization_drainer.await.map_err(|error| {
                crate::errors::FollowerStackError::ExecutorTaskFailed(format!("{error:?}"))
            })?;
            Ok::<_, eyre::Report>(follower_shutdown)
        };
        let (_, follower_shutdown) = futures::try_join!(follow_engine, accepted_work)?;
        follower_shutdown.complete()
    }
}
#[derive(Debug, thiserror::Error)]
enum BootstrapPromotionError {
    #[error("upstream finalized tip unavailable before validator bootstrap")]
    UpstreamTipUnavailable,
}

struct FinalityObserver {
    protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
    observer_mailbox: MarshalMailbox,
    observer_node: OutbeFullNode,
    observer_schemes: HybridSchemeProvider<MinSig>,
    observer_store: outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    observer_bridge: ConsensusExecutionBridge,
    observer_ingress: crate::follower_shutdown::FollowerReporter,
    execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
    bootstrap: Option<ValidatorBootstrap>,
    initial_upstream_tip: u64,
    initial_persisted_height: u64,
    driver_cancel: tokio::sync::watch::Sender<bool>,
    finalization_notify: Arc<tokio::sync::Notify>,
}
impl FinalityObserver {
    async fn run(mut self) -> Result<()> {
        let mut last_persisted = Some(self.initial_persisted_height);
        loop {
            self.maybe_promote_bootstrap_validator(last_persisted.unwrap_or(0))
                .await?;
            let height = tokio::select! {
                height = self.execution_finalized_height_rx.recv() => {
                    let Some(height) = height else { break; };
                    height
                },
                _ = wait_for_bootstrap_tee_lease(self.bootstrap.as_mut()) => continue,
            };
            if !should_persist_finalized_height(&self.observer_mailbox, height).await? {
                continue;
            }
            FollowerProofPersistence {
                protocol: self.protocol.as_ref(),
                node: &self.observer_node,
                certificate_scheme_provider: &self.observer_schemes,
                parent_cert_store: &self.observer_store,
            }
            .reconcile_height(&self.observer_mailbox, height)
            .await?;
            self.observer_bridge.set_last_finalized_block_number(height);
            last_persisted = Some(height);
            self.finalization_notify.notify_waiters();
        }
        verify_finality_observer_quiesced(
            &self.observer_ingress,
            &self.observer_mailbox,
            last_persisted,
        )
        .await
    }

    async fn maybe_promote_bootstrap_validator(&mut self, persisted: u64) -> Result<()> {
        let Some(readiness) = self.bootstrap.as_ref() else {
            return Ok(());
        };
        let Some(processed) = self.observer_mailbox.get_processed_height().await else {
            return Ok(());
        };
        if processed.get() >= self.initial_upstream_tip
            && persisted >= self.initial_upstream_tip
            && readiness.is_validator_bootstrap_ready(&self.observer_node, persisted)?
        {
            self.observer_ingress.initiate_promotion(persisted)?;
            self.driver_cancel.send_replace(true);
            self.bootstrap = None;
        }
        Ok(())
    }
}

pub(in crate::stack) async fn wait_for_bootstrap_tee_lease(
    bootstrap: Option<&mut ValidatorBootstrap>,
) {
    match bootstrap {
        Some(readiness) => {
            if readiness.tee_lease_armed_rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

async fn should_persist_finalized_height(
    observer_mailbox: &MarshalMailbox,
    height: u64,
) -> Result<bool> {
    if !follower_height_has_certified_finalization(height) {
        return Ok(false);
    }
    if observer_mailbox
        .get_finalization(Height::new(height))
        .await
        .is_some()
    {
        return Ok(true);
    }
    if observer_mailbox
        .get_info(Height::new(height))
        .await
        .is_none()
    {
        return Err(
            crate::errors::FollowerStackError::MarshalLostFinalizedBlockBeforeDrain(height).into(),
        );
    }
    Ok(false)
}

async fn verify_finality_observer_quiesced(
    observer_ingress: &crate::follower_shutdown::FollowerReporter,
    observer_mailbox: &MarshalMailbox,
    last_persisted: Option<u64>,
) -> Result<()> {
    if !observer_ingress.is_quiescing()? {
        return Err(crate::errors::FollowerStackError::FinalityObserverStoppedBeforeQuiesce.into());
    }
    if let Some(height) = last_persisted.filter(|height| *height > 0) {
        let processed = observer_mailbox
            .get_processed_height()
            .await
            .ok_or(crate::errors::FollowerStackError::MarshalUnavailableBeforeDrainCompleted)?;
        if processed.get() < height {
            return Err(
                crate::errors::FollowerStackError::MarshalHasNotAcknowledgedProofDrain {
                    processed,
                    persisted: height,
                }
                .into(),
            );
        }
    }
    Ok(())
}
