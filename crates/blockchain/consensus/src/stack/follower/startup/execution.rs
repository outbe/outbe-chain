use super::super::super::*;
use super::super::{
    follower_height_has_certified_finalization, spawn_finalization_drainer,
    FollowerProofPersistence, ValidatorBootstrap,
};
use super::{FollowerRuntime, RecoveredFollowerHistory, StartupChainState};
use crate::follow::{run_follow_engine, FollowEngineConfig, TipSource as _};
use crate::marshal_types::MarshalMailbox;
use crate::stack::recovery::anchor::CertifiedFollowerRecoveryAnchor;

use services::FollowerStackServices;

pub struct ExecutionBootstrap<E: FollowerRuntime, N: StackNodeAccess> {
    pub ctx: E,
    pub node: N,
    pub bridge: ConsensusExecutionBridge,
    pub history: RecoveredFollowerHistory<E, N>,
    pub startup: StartupChainState,
    pub services: FollowerStackServices,
}
struct ConfirmedExecutor<E: FollowerRuntime> {
    actor: ExecutorActor<E>,
    mailbox: crate::executor::Mailbox,
    finalized_heights: tokio::sync::mpsc::UnboundedReceiver<u64>,
}
struct ParentCertificateBootstrap<'a, N: StackNodeAccess> {
    protocol: &'a dyn crate::protocol_state::EngineProtocolState,
    node: &'a N,
    store: crate::finalization::parent_cert_store::FinalizedParentCertStore,
    recovery_anchor: &'a CertifiedFollowerRecoveryAnchor,
    certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
}
struct RecoveredExecutionBootstrap<'a> {
    bridge: &'a ConsensusExecutionBridge,
    recovery_anchor: &'a CertifiedFollowerRecoveryAnchor,
    last_consensus_finalized: Height,
    last_execution_height: u64,
}
impl<E: FollowerRuntime, N: StackNodeAccess> ExecutionBootstrap<E, N> {
    async fn confirmed_executor(&self) -> Result<ConfirmedExecutor<E>> {
        let ctx = &self.ctx;
        let node = &self.node;
        let genesis_hash = self.startup.genesis_hash;
        let recovery_anchor = &self.history.recovery_anchor;
        let engine_handle: EngineHandle = node.beacon_engine_handle().engine_handle();
        let (execution_finalized_height_tx, execution_finalized_height_rx) =
            tokio::sync::mpsc::unbounded_channel::<u64>();
        let (executor_actor, executor_mailbox) = ExecutorActor::new(
            ctx.child("executor"),
            engine_handle,
            crate::executor::actor::RecoveredFinalizedState {
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
            move || Ok(fcu_provider_node.recovery_forkchoice()?.into()),
        )
        .await
        .wrap_err("failed to confirm recovered Reth forkchoice before follower startup")?;

        Ok(ConfirmedExecutor {
            actor: executor_actor,
            mailbox: executor_mailbox,
            finalized_heights: execution_finalized_height_rx,
        })
    }
    pub async fn run(self) -> Result<crate::follower_shutdown::FollowStackExit> {
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
impl<N: StackNodeAccess> ParentCertificateBootstrap<'_, N> {
    fn install(self) -> Result<crate::finalization::parent_cert_store::FinalizedParentCertStore> {
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
struct LiveFollower<E: FollowerRuntime, N: StackNodeAccess> {
    ctx: E,
    node: N,
    bridge: ConsensusExecutionBridge,
    history: RecoveredFollowerHistory<E, N>,
    finalized_parent_cert_store: crate::finalization::parent_cert_store::FinalizedParentCertStore,
    executor_reporter: crate::follower_shutdown::FollowerReporter,
    observer_ingress: crate::follower_shutdown::FollowerReporter,
    executor_handle: commonware_runtime::Handle<Result<()>>,
    execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
    follower_shutdown: crate::follower_shutdown::FollowerDrain,
    bootstrap: Option<ValidatorBootstrap>,
    finalization_notify: Arc<tokio::sync::Notify>,
    protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
}
impl<E: FollowerRuntime, N: StackNodeAccess> LiveFollower<E, N> {
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
                .map_err(|err| {
                    tracing::warn!(
                        error = outbe_primitives::telemetry::error_field(&err),
                        "initial upstream finalized tip unavailable"
                    );
                    BootstrapPromotionError::UpstreamTipUnavailable
                })?
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

struct FinalityObserver<N: StackNodeAccess> {
    protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
    observer_mailbox: MarshalMailbox,
    observer_node: N,
    observer_schemes: HybridSchemeProvider<MinSig>,
    observer_store: crate::finalization::parent_cert_store::FinalizedParentCertStore,
    observer_bridge: ConsensusExecutionBridge,
    observer_ingress: crate::follower_shutdown::FollowerReporter,
    execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
    bootstrap: Option<ValidatorBootstrap>,
    initial_upstream_tip: u64,
    initial_persisted_height: u64,
    driver_cancel: tokio::sync::watch::Sender<bool>,
    finalization_notify: Arc<tokio::sync::Notify>,
}
impl<N: StackNodeAccess> FinalityObserver<N> {
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
        let Some(processed) = self
            .observer_mailbox
            .get_processed()
            .await
            .map(commonware_consensus::marshal::core::Processed::height)
        else {
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

pub async fn wait_for_bootstrap_tee_lease(bootstrap: Option<&mut ValidatorBootstrap>) {
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
            .get_processed()
            .await
            .map(commonware_consensus::marshal::core::Processed::height)
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

#[cfg(test)]
mod processed_absent_tests {
    use super::*;
    use commonware_consensus::{
        marshal::{self, core::Processed, resolver::handler},
        simplex::types::{Finalization, Finalize, Proposal},
        types::{Epoch, Round, View, ViewDelta},
    };
    use commonware_cryptography::bls12381::{self, primitives::variant::MinSig};
    use commonware_parallel::Sequential;
    use commonware_runtime::{
        buffer::paged::CacheRef, deterministic, Runner as _, Supervisor as _,
    };
    use commonware_utils::{
        ordered::{Quorum as _, Set},
        Acknowledgement as _,
    };
    use reth_ethereum::{primitives::SealedBlock, Block};
    use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    struct HoldingReporter {
        held: Arc<Mutex<Vec<(u64, commonware_utils::acknowledgement::Exact)>>>,
    }

    impl commonware_consensus::Reporter for HoldingReporter {
        type Activity = crate::marshal_types::MarshalUpdate;
        fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
            if let marshal::Update::Block(block, ack) = activity {
                if block.inner().number() == 0 {
                    ack.acknowledge();
                } else {
                    self.held
                        .lock()
                        .unwrap()
                        .push((block.inner().number(), ack));
                }
            }
            commonware_actor::Feedback::Ok
        }
    }

    #[test]
    fn finality_observer_quiesce_enforces_processed_absent_floor_then_accepts_block() {
        deterministic::Runner::default().start(|context| async move {
            let keys: Vec<bls12381::PrivateKey> =
                (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
            let participants: Set<bls12381::PublicKey> =
                keys.iter().map(|k| k.public_key()).try_collect().unwrap();
            let dkg = crate::bls::bootstrap_dkg(3).unwrap();
            let signers: Vec<crate::hybrid::HybridScheme<MinSig>> = keys
                .iter()
                .map(|key| {
                    let idx = participants.index(&key.public_key()).unwrap();
                    crate::hybrid::HybridScheme::signer(
                        &crate::config::outbe_app_namespace(),
                        participants.clone(),
                        key.clone(),
                        dkg.polynomial.clone(),
                        dkg.shares[idx.get() as usize].clone(),
                    )
                    .unwrap()
                })
                .collect();
            let verifier = crate::hybrid::HybridScheme::<MinSig>::verifier(
                &crate::config::outbe_app_namespace(),
                participants,
                dkg.polynomial.clone(),
            )
            .unwrap();
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier.clone()));

            let mut raw = Block::default();
            raw.header.number = 5;
            raw.header.extra_data = alloy_primitives::Bytes::from_static(&[5]);
            let floor_block = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(outbe_primitives::OutbeHeader::new),
            ));
            let round = Round::new(Epoch::new(0), View::new(5));
            let proposal = Proposal::new(round, View::new(4), floor_block.digest());
            let votes: Vec<_> = signers
                .iter()
                .map(|s| Finalize::sign(s, proposal.clone()).unwrap())
                .collect();
            let finalization = Finalization::from_finalizes(
                &verifier,
                commonware_utils::iter::NonEmpty::try_new(votes.iter()).unwrap(),
                &Sequential,
            )
            .unwrap();

            let cache = CacheRef::new(
                NonZeroU16::new(1024).unwrap(),
                NonZeroUsize::new(10).unwrap(),
            );
            let items = NonZeroU64::new(10).unwrap();
            let buffer = NonZeroUsize::new(1024).unwrap();
            let (certificates, blocks) = crate::test_fixtures::marshal::MarshalArchiveFixture {
                partition_prefix: "quiesce-absent",
                page_cache: &cache,
                items_per_section: items,
                replay_buffer: buffer,
                write_buffer: buffer,
            }
            .open(&context)
            .await
            .unwrap();
            let genesis = Block::default().map_header(outbe_primitives::OutbeHeader::new);
            let (actor, mailbox, _) = marshal::core::Actor::init(
                context.child("marshal"),
                certificates,
                blocks,
                marshal::Config {
                    provider,
                    epocher: crate::follow::FollowerEpocher::new(100, 10),
                    start: marshal::Start::Genesis(crate::test_fixtures::digested_block(
                        crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(genesis)),
                    )),
                    partition_prefix: "quiesce-absent".into(),
                    mailbox_size: NonZeroUsize::new(32).unwrap(),
                    view_retention: ViewDelta::new(100),
                    prunable_items_per_section: items,
                    page_cache: cache,
                    replay_buffer: buffer,
                    key_write_buffer: buffer,
                    value_write_buffer: buffer,
                    block_codec_config: (),
                    max_repair: NonZeroUsize::new(10).unwrap(),
                    max_pending_acks: NonZeroUsize::new(1).unwrap(),
                    strategy: Sequential,
                },
            )
            .await;
            let held = Arc::new(Mutex::new(Vec::new()));
            let (receiver, _handler) =
                handler::init(context.child("handler"), NonZeroUsize::new(32).unwrap());
            let resolver = crate::test_harness::NoopMarshalResolver;
            let _marshal_handle = actor.start_unbuffered(
                HoldingReporter {
                    held: Arc::clone(&held),
                },
                (receiver, resolver),
            );
            while mailbox.get_processed().await != Some(Processed::Block(Height::zero())) {
                context.sleep(Duration::from_millis(1)).await;
            }
            assert!(
                mailbox
                    .verified(
                        round,
                        commonware_cryptography::Digested::precomputed(
                            floor_block.clone(),
                            floor_block.digest(),
                        ),
                    )
                    .await
            );
            mailbox.set_floor(finalization);
            while mailbox.get_processed().await != Some(Processed::Absent(Height::new(4))) {
                context.sleep(Duration::from_millis(1)).await;
            }

            let (_control, drain) = crate::follower_shutdown::follower_drain_pair();
            let (tx, _rx) = futures::channel::mpsc::unbounded();
            let reporter = drain
                .install(crate::executor::Mailbox::from_sender(tx))
                .unwrap()
                .unwrap();
            reporter.initiate_promotion(5).unwrap();

            // At Processed::Absent(4): persisted == 4 is acknowledged, persisted == 5 fails closed.
            verify_finality_observer_quiesced(&reporter, &mailbox, Some(4))
                .await
                .expect("persisted == Absent(4) floor must quiesce cleanly");
            let err = verify_finality_observer_quiesced(&reporter, &mailbox, Some(5))
                .await
                .expect_err("persisted > Absent(4) must fail quiesce");
            let typed = err
                .downcast_ref::<crate::errors::FollowerStackError>()
                .expect("typed FollowerStackError");
            assert!(
                matches!(
                    typed,
                    crate::errors::FollowerStackError::MarshalHasNotAcknowledgedProofDrain {
                        processed,
                        persisted: 5,
                    } if *processed == Height::new(4)
                ),
                "unexpected error: {typed:?}"
            );

            // Acknowledging block 5 advances Processed::Absent(4) -> Processed::Block(5),
            // allowing persisted == 5 to quiesce cleanly.
            while held.lock().unwrap().is_empty() {
                context.sleep(Duration::from_millis(1)).await;
            }
            for (_, ack) in held.lock().unwrap().drain(..) {
                ack.acknowledge();
            }
            while mailbox.get_processed().await != Some(Processed::Block(Height::new(5))) {
                context.sleep(Duration::from_millis(1)).await;
            }
            verify_finality_observer_quiesced(&reporter, &mailbox, Some(5))
                .await
                .expect("persisted == Block(5) must quiesce cleanly");
        });
    }
}
