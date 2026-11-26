//! Preserve select ordering while delegating domain events to epoch operations.
use super::super::*;
use super::runtime::*;
use futures::FutureExt as _;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

impl<E, N: StackNodeAccess> EpochSupervisor<E, N>
where
    E: Clock + CryptoRng + Network + Resolver + Spawner + Storage + Metrics + Send + Sync + 'static,
{
    pub(super) fn publish_repository_material(&self) {
        if let Some(publisher) = &self.repository_material_publisher {
            publisher.publish(
                self.state.current_epoch.get(),
                self.state.vrf_material_version,
                self.state.last_dkg_output.as_ref(),
            );
        }
    }

    pub(super) async fn run(
        mut self,
        ctx: E,
        recovered_pending_boundary: Option<PendingDkgBoundarySnapshot>,
        recovery_anchor_height: u64,
    ) -> Result<()> {
        if self
            .restore_pending(&ctx, recovered_pending_boundary, recovery_anchor_height)
            .await?
            == super::pending_recovery::PendingRestoreOutcome::Stopped
        {
            info!("global stop received during pending DKG startup channel registration; exiting");
            return Ok(());
        }
        self.publish_repository_material();
        let mut authority = self.protocol.tee_authority_watch();
        let mut latest_consensus_tip;
        let mut pending_provider_ready_height: Option<u64> = None;
        let mut watchdog_unhealthy_since: Option<SystemTime> = None;
        let watchdog_started_at = ctx.current();
        let mut provider_ready_retry_timer: Pin<Box<dyn Future<Output = ()> + Send>> =
            Box::pin(std::future::pending());
        let mut execution_watchdog_timer: Pin<Box<dyn Future<Output = ()> + Send>> =
            Box::pin(ctx.sleep(config::EXECUTION_WATCHDOG_INTERVAL));
        info!(
            epoch_length_blocks = self.dkg_rotation_params.epoch_length_blocks,
            prepare_window_blocks = self.dkg_rotation_params.prepare_window_blocks,
            activation_grace_blocks = self.dkg_rotation_params.activation_grace_blocks,
            "configured block-based DKG/VRF rotation"
        );
        info!(
            leader_timeout = ?self.bt.leader_timeout,
            certification_timeout = ?self.bt.certification_timeout,
            "consensus timeouts (genesis-sourced, no CLI override)"
        );
        'epoch_loop: loop {
            // -- a. Register or take pre-registered epoch sub-channels -------
            // Activation pre-registers `next_epoch_subchannels` at DKG
            // completion (see DKG completion handler below). The top of the
            // next iteration consumes it. The fallback path covers the
            // genesis-bootstrap iteration where no prior DKG completion ran.
            let Some(current_subchannels) = self.acquire_epoch_subchannels(&ctx).await? else {
                info!(
                    epoch = %self.state.current_epoch,
                    "global stop received during epoch subchannel acquisition; exiting"
                );
                break 'epoch_loop;
            };

            let accountable = Arc::new(AtomicBool::new(self.local_validator_accountable()));
            let (mut engine_handle_task, signer_enabled) = self
                .start_engine(&ctx, current_subchannels, accountable.clone())
                .await?;
            latest_consensus_tip = *self.consensus_tip_rx.borrow_and_update();
            if let Some(tip) = latest_consensus_tip {
                self.finalization_mailbox.reconcile_marshal_tip(tip)?;
            }
            // -- g. Engine event loop ----------------------------------------
            // Monitors engine, component exits, and block-height-driven reshare triggers.

            let epoch_loop_result: Result<EpochLoopOutcome> = async {
            let mut stack_shutdown = ctx.stopped();

            loop {
            let accountable_validator = accountable.load(Ordering::Acquire);
            let reshare_active = self.rotation.reshare_in_progress;
            let wait_for_execution_finalized_height = async {
                if reshare_active {
                    std::future::pending::<Option<u64>>().await
                } else {
                    self.execution_finalized_height_rx.recv().await
                }
            };
            commonware_macros::select! {
                _ = &mut stack_shutdown => {
                    info!(epoch = %self.state.current_epoch, "global stop received; draining simplex engine");
                    return Ok(EpochLoopOutcome::GlobalStop);
                },

                _ = &mut self.actors.network_handle => {
                    info!("P2P network exited");
                    return Ok(EpochLoopOutcome::StackExit);
                },

                desired_signer = super::signer::wait_for_authority_role_change(
                    &mut self.radicle_updates,
                    &mut authority,
                    signer_enabled,
                    accountable_validator,
                    self.state.signing_share.is_some(),
                ) => {
                    let desired_signer = desired_signer?;
                    info!(
                        epoch = %self.state.current_epoch,
                        previous_signer = signer_enabled,
                        desired_signer,
                        "local TEE or canonical voting gate changed; replacing same-epoch Simplex role"
                    );
                    return Ok(EpochLoopOutcome::ReplaceSigner);
                },

                // Engine exit -> clean shutdown.
                result = &mut engine_handle_task => {
                    info!(epoch = %self.state.current_epoch, "simplex engine exited");
                    return Ok(EpochLoopOutcome::EngineExit(result));
                },

                // DKG reshare completed (from background task).
                Some(dkg_result) = self.rotation.dkg_result_rx.recv() => {
                    if let EventAction::Outcome(outcome) = self.complete_dkg(&ctx, dkg_result).await? { return Ok(outcome); }
                },

                Some(progress) = self.rotation.dkg_progress_rx.recv() => {
                    match progress {
                        dkg_actor::DkgProgress::LocalDealerLog(bytes) => {
                            if let Err(error) = self.dkg_manager.note_local_dealer_log(self.state.current_epoch, bytes) {
                                warn!(%error, epoch = %self.state.current_epoch, "failed recording local dealer log");
                            }
                        }
                        dkg_actor::DkgProgress::P2pDealerLog(bytes) => {
                            if let Err(error) = self.dkg_manager.note_pending_dealer_log(self.state.current_epoch, bytes) {
                                warn!(%error, epoch = %self.state.current_epoch, "failed recording P2P dealer log candidate");
                            }
                        }
                    }
                },

                _ = &mut execution_watchdog_timer => {
                    execution_watchdog_timer =
                        Box::pin(ctx.sleep(config::EXECUTION_WATCHDOG_INTERVAL));
                    self.check_execution(&ctx, latest_consensus_tip, watchdog_started_at, &mut watchdog_unhealthy_since)?;
                },

                consensus_tip_changed = self.consensus_tip_rx.changed() => {
                    match consensus_tip_changed {
                        Ok(()) => {
                            latest_consensus_tip = *self.consensus_tip_rx.borrow_and_update();
                            if let Some(tip) = latest_consensus_tip {
                                self.finalization_mailbox.reconcile_marshal_tip(tip)?;
                            }
                            accountable.store(self.local_validator_accountable(), Ordering::Release);
                            let desired_signer = super::signer::authority_signer_enabled(
                                authority.as_ref(), accountable.load(Ordering::Acquire), self.state.signing_share.is_some(), self.radicle_status.snapshot().voting_gate,
                            )?;
                            if desired_signer != signer_enabled {
                                info!(epoch = %self.state.current_epoch, previous_signer = signer_enabled, desired_signer,
                                    "canonical tip changed validator eligibility; replacing same-epoch Simplex role");
                                return Ok(EpochLoopOutcome::ReplaceSigner);
                            }
                            if let Some(current_height) = pending_provider_ready_height {
                                let _ = self.execution_finalized_height_tx.send(current_height);
                            }
                            // The height arm is off while a reshare runs.
                            if self.rotation.reshare_in_progress
                                && self.promote_boundary(RetireScope::PendingMaterialOnly).await? == BoundaryPromotion::LocalExcluded
                            {
                                return Ok(EpochLoopOutcome::StackExit);
                            }
                        }
                        Err(error) => {
                            warn!(%error, "consensus tip watch channel closed");
                        }
                    }
                },

                Some(view_height) = self.finalization_view_advanced_rx.recv() => {
                    let _ = self.execution_finalized_height_tx.send(view_height);
                },

                _ = &mut provider_ready_retry_timer => {
                    provider_ready_retry_timer = Box::pin(std::future::pending());
                    if let Some(current_height) = pending_provider_ready_height {
                        let _ = self.execution_finalized_height_tx.send(current_height);
                    }
                },

                // Block-height based DKG/VRF rotation. Execution and canonical
                // finalization-view notifications both recheck the provider/tip gate.
                Some(current_height) = wait_for_execution_finalized_height => {
                    match latest_consensus_tip {
                        Some(tip) => {
                            if !provider_matches_consensus_tip(self.node.provider(), tip, current_height)? {
                                pending_provider_ready_height = Some(current_height);
                                provider_ready_retry_timer =
                                    Box::pin(ctx.sleep(config::DEFAULT_PEER_RESPONSE_TIMEOUT));
                                debug!(
                                    current_height,
                                    consensus_tip_height = tip.height.get(),
                                    consensus_tip_digest = %tip.digest,
                                    "provider not ready for DKG/VRF scheduling; retrying"
                                );
                                continue;
                            }
                            pending_provider_ready_height = None;
                            provider_ready_retry_timer = Box::pin(std::future::pending());
                            if self.promote_boundary(RetireScope::All).await? == BoundaryPromotion::LocalExcluded { return Ok(EpochLoopOutcome::StackExit); }
                        }
                        None => {
                            pending_provider_ready_height = Some(current_height);
                            debug!(
                                current_height,
                                "no consensus tip available for DKG/VRF scheduling; retrying"
                            );
                            continue;
                        }
                    }

                    // A consensus tip can precede its execution state; refresh after provider catch-up too.
                    accountable.store(self.local_validator_accountable(), Ordering::Release);
                    match self.activate_pending(&ctx, current_height, &mut engine_handle_task).await? {
                        EventAction::Outcome(outcome) => return Ok(outcome),
                        EventAction::Continue => continue,
                        EventAction::Proceed => {}
                    }
                    if let EventAction::Outcome(outcome) = self.schedule_rotation(&ctx, current_height).await? { return Ok(outcome); }
                },

                backup_vote = self.vote_backup_rx.recv() => {
                    let Some((their_epoch, (from, _))) = backup_vote else {
                        continue;
                    };
                    if let Some(hint_height) = future_epoch_hint_height(
                        &self.epocher,
                        self.state.current_epoch,
                        Epoch::new(their_epoch),
                    ) {
                        self.marshal_mailbox.hint_finalized(
                            hint_height,
                            commonware_utils::vec::NonEmptyVec::new(from),
                        );
                    }
                },

                // Component exits -> fatal.
                result = await_supervised_component_exit(
                    &mut self.actors.executor_handle_task,
                    &mut self.actors.handler_handle,
                    &mut self.actors.finalization_handle,
                ) => return result,
                result = &mut self.actors.peer_manager_handle_task => {
                    info!("peer manager actor exited");
                    result.map_err(crate::errors::StackEpochError::peer_manager_actor_exited)?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                // SSA-8: the marshal actor is consensus-liveness-critical (block
                // availability, finalized-block delivery to the executor). With
                // `catch_panics`, a marshal panic (e.g. an unacknowledged Exact,
                // or a future telemetry-label assert) resolves its handle instead
                // of aborting the process. So an UNmonitored handle would leave
                // the node silently stalled (no blocks delivered, consensus
                // wedged). Monitor it like the other components: a marshal exit
                // is fatal and shuts the node down with the cause.
                result = &mut self.actors.marshal_handle => {
                    info!("marshal actor exited");
                    result.map_err(crate::errors::StackEpochError::marshal_actor_exited)?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                // The Commonware runtime still manages the broadcast (buffered
                // dissemination) handle. Its failure degrades to the marshal
                // pull/serve path rather than a consensus stall.
            }
            }
        }
        .await;

            match supervise_epoch_loop_result(
                &ctx,
                epoch_loop_result,
                &mut engine_handle_task,
                &self.application_drain,
            )
            .await?
            {
                EpochLoopAction::RestartEpoch => continue 'epoch_loop,
                EpochLoopAction::ReplaceSigner => {
                    let current_epoch = self.state.current_epoch;
                    let Some(channels) = reacquire_replacement_channels(
                        &ctx,
                        crate::epoch_subchannels::reacquire_epoch_subchannels(
                            current_epoch,
                            &ctx,
                            crate::epoch_subchannels::SubchannelRetryPolicy {
                                timeout: Duration::from_secs(5),
                                retry_interval: Duration::from_millis(10),
                            },
                            crate::epoch_subchannels::EpochMuxHandles {
                                vote: &mut self.channels.vote_mux,
                                cert: &mut self.channels.cert_mux,
                                res: &mut self.channels.res_mux,
                            },
                        ),
                    )
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "reacquire same-epoch channels while replacing signer role in epoch {current_epoch}"
                        )
                    })?
                    else {
                        info!(epoch = %current_epoch, "global stop received while replacing same-epoch Simplex role");
                        break 'epoch_loop;
                    };
                    self.channels.replacement_epoch_subchannels = Some(channels);
                    continue 'epoch_loop;
                }
                EpochLoopAction::ExitStack => break 'epoch_loop,
            }
        }

        Ok(())
    }

    async fn acquire_epoch_subchannels(
        &mut self,
        ctx: &E,
    ) -> Result<Option<super::transport::EpochSubchannels<E>>> {
        let current_subchannels = if self.channels.replacement_epoch_subchannels.is_some() {
            crate::epoch_subchannels::take_or_register_current(
                self.state.current_epoch,
                &mut self.channels.replacement_epoch_subchannels,
                &mut self.channels.vote_mux,
                &mut self.channels.cert_mux,
                &mut self.channels.res_mux,
            )
            .await?
        } else if self.channels.next_epoch_subchannels.is_some() {
            crate::epoch_subchannels::take_or_register_current(
                self.state.current_epoch,
                &mut self.channels.next_epoch_subchannels,
                &mut self.channels.vote_mux,
                &mut self.channels.cert_mux,
                &mut self.channels.res_mux,
            )
            .await?
        } else {
            let Some(channels) = poll_mux_registration_until_stopped(
                ctx,
                crate::epoch_subchannels::take_or_register_current(
                    self.state.current_epoch,
                    &mut self.channels.next_epoch_subchannels,
                    &mut self.channels.vote_mux,
                    &mut self.channels.cert_mux,
                    &mut self.channels.res_mux,
                ),
            )
            .await?
            else {
                return Ok(None);
            };
            channels
        };
        if let Some(pending_epoch) = self.rotation.deferred_startup_pending_epoch.take() {
            if self.channels.next_epoch_subchannels.is_some() {
                return Err(
                    crate::errors::StackEpochError::RecoveredFutureDkgHandoffSubchannelCollision
                        .into(),
                );
            }
            let current_epoch = self.state.current_epoch;
            let Some(next_subchannels) = poll_mux_registration_until_stopped(
                ctx,
                crate::epoch_subchannels::register_epoch_subchannels(
                    pending_epoch,
                    &mut self.channels.vote_mux,
                    &mut self.channels.cert_mux,
                    &mut self.channels.res_mux,
                ),
            )
            .await
            .wrap_err_with(|| {
                format!(
                    "pre-register recovered future-epoch subchannels after acquiring current epoch {current_epoch}"
                )
            })?
            else {
                return Ok(None);
            };
            self.channels.next_epoch_subchannels = Some(next_subchannels);
            info!(
                active_epoch = %self.state.current_epoch,
                pending_epoch = %pending_epoch,
                "pre-registered recovered future DKG channels after current epoch"
            );
        }
        Ok(Some(current_subchannels))
    }

    pub(super) async fn promote_boundary(
        &mut self,
        scope: RetireScope,
    ) -> Result<BoundaryPromotion> {
        let finalized_height = self.finalization_view.read().last_finalized_number;
        adopt_finalized_boundary_carrier(
            &self.dkg_manager,
            self.node.provider(),
            self.state.last_dkg_activation_height,
            finalized_height,
        )?;
        let local_key = self.signing_key.public_key();
        let active = ActiveDkgMaterial {
            local_key: &local_key,
            output: self.state.last_dkg_output.as_ref(),
            share: self.state.signing_share.as_ref(),
            polynomial: &self.state.polynomial,
        };
        promote_committed_boundary(
            &self.dkg_manager,
            self.args.keys_dir.as_deref(),
            &self.key_backend,
            active,
            scope,
        )
        .await
    }
}

async fn await_supervised_component_exit(
    executor: &mut commonware_runtime::Handle<Result<()>>,
    application_handler: &mut commonware_runtime::Handle<Result<()>>,
    finalization_actor: &mut commonware_runtime::Handle<
        std::result::Result<(), outbe_primitives::error::PrecompileError>,
    >,
) -> Result<EpochLoopOutcome> {
    commonware_macros::select! {
        result = executor => Err(crate::supervision::unexpected_eyre_actor_exit("executor", result)),
        result = application_handler => Err(crate::supervision::unexpected_eyre_actor_exit("application_handler", result)),
        result = finalization_actor => Err(crate::supervision::unexpected_fallible_actor_exit("finalization_actor", result)),
    }
}

pub fn future_epoch_hint_height(
    epocher: &crate::follow::FollowerEpocher,
    current_epoch: Epoch,
    their_epoch: Epoch,
) -> Option<Height> {
    if their_epoch <= current_epoch {
        return None;
    }
    epocher.backup_vote_hint_height(current_epoch)
}

pub(super) async fn poll_mux_registration_until_stopped<T>(
    ctx: &impl Spawner,
    register: impl Future<Output = Result<T>>,
) -> Result<Option<T>> {
    let mut stack_shutdown = ctx.stopped();
    tokio::select! {
        biased;
        _ = &mut stack_shutdown => Ok(None),
        result = register => {
            // Stop may become ready during the registration future's poll.
            if (&mut stack_shutdown).now_or_never().is_some() {
                return Ok(None);
            }
            result.map(Some)
        }
    }
}

async fn reacquire_replacement_channels<T>(
    ctx: &impl Spawner,
    reacquire: impl Future<Output = Result<T>>,
) -> Result<Option<T>> {
    poll_mux_registration_until_stopped(ctx, reacquire).await
}

#[cfg(test)]
mod tests {
    use super::{await_supervised_component_exit, reacquire_replacement_channels};
    use commonware_p2p::utils::mux::Error as MuxError;
    use commonware_runtime::{deterministic, Runner as _, Spawner as _, Supervisor as _};
    use futures::FutureExt as _;

    #[test]
    fn signer_replacement_propagates_closed_mux_while_stack_is_live() {
        deterministic::Runner::default().start(|ctx| async move {
            let error = reacquire_replacement_channels(&ctx, async {
                Err::<(), _>(MuxError::Closed.into())
            })
            .await
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<MuxError>(),
                Some(MuxError::Closed)
            ));
        });
    }

    #[test]
    fn signer_replacement_prefers_stack_stop_to_closed_mux() {
        for stop_before_reacquire in [true, false] {
            deterministic::Runner::default().start(move |ctx| async move {
                let stop_ctx = ctx.child("replacement_stop");
                if stop_before_reacquire {
                    ctx.child("early_stop").stop(0, None).await.unwrap();
                }
                let channels = reacquire_replacement_channels(&ctx, async move {
                    assert!(
                        !stop_before_reacquire,
                        "reacquire was polled after global stop"
                    );
                    // Signal the stop after select checked it but before returning the mux error.
                    let _ = stop_ctx.stop(0, None).now_or_never();
                    Err::<(), _>(MuxError::Closed.into())
                })
                .await
                .expect("global stop must win over a closed mux");
                assert_eq!(channels, None);
            });
        }
    }

    #[test]
    fn signer_replacement_cancels_pending_reacquisition_on_stack_stop() {
        deterministic::Runner::default().start(|ctx| async move {
            let stop_ctx = ctx.child("replacement_stop");
            let channels = reacquire_replacement_channels(&ctx, async move {
                let _ = stop_ctx.stop(0, None).now_or_never();
                std::future::pending::<eyre::Result<()>>().await
            })
            .await
            .unwrap();
            assert_eq!(channels, None);
        });
    }

    #[test]
    fn signer_replacement_prefers_stack_stop_to_successful_reacquisition() {
        deterministic::Runner::default().start(|ctx| async move {
            let live = reacquire_replacement_channels(&ctx, async { Ok(()) })
                .await
                .unwrap();
            assert_eq!(live, Some(()));

            let stop_ctx = ctx.child("replacement_stop");
            let stopping = reacquire_replacement_channels(&ctx, async move {
                let _ = stop_ctx.stop(0, None).now_or_never();
                Ok(())
            })
            .await
            .unwrap();
            assert_eq!(stopping, None);
        });
    }

    async fn complete_only<E>(actor: &'static str, exiting: &'static str) -> Result<(), E> {
        if actor != exiting {
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    #[test]
    fn early_successful_component_exit_is_fatal() {
        for actor in ["executor", "application_handler", "finalization_actor"] {
            deterministic::Runner::default().start(move |context| async move {
                let mut executor = context
                    .child("executor")
                    .spawn(move |_| complete_only("executor", actor));
                let mut application_handler = context
                    .child("application_handler")
                    .spawn(move |_| complete_only("application_handler", actor));
                let mut finalization_actor = context
                    .child("finalization_actor")
                    .spawn(move |_| complete_only("finalization_actor", actor));
                let outcome = await_supervised_component_exit(
                    &mut executor,
                    &mut application_handler,
                    &mut finalization_actor,
                )
                .await;
                let error = outcome.expect_err("early component exit must be fatal");
                let Some(crate::supervision::SupervisedActorError::UnexpectedExit { actor: name }) =
                    error.downcast_ref()
                else {
                    panic!("unexpected error: {error:#}");
                };
                assert_eq!(*name, actor);
            });
        }
    }

    type SimMuxHandle = commonware_p2p::utils::mux::MuxHandle<
        commonware_p2p::simulated::Sender<
            commonware_cryptography::bls12381::PublicKey,
            deterministic::Context,
        >,
    >;
    type SimMuxer = commonware_p2p::utils::mux::Muxer<
        deterministic::Context,
        commonware_p2p::simulated::Sender<
            commonware_cryptography::bls12381::PublicKey,
            deterministic::Context,
        >,
        commonware_p2p::simulated::Receiver<commonware_cryptography::bls12381::PublicKey>,
    >;
    type SimMuxTaskHandle =
        commonware_runtime::Handle<std::result::Result<(), commonware_p2p::simulated::Error>>;

    struct TestEpochMuxes {
        vote: SimMuxHandle,
        cert: SimMuxHandle,
        res: SimMuxHandle,
        cert_handle: Option<SimMuxTaskHandle>,
        _held_cert_muxer: Option<SimMuxer>,
    }

    async fn setup_epoch_muxes(
        ctx: &deterministic::Context,
        start_cert_muxer: bool,
    ) -> TestEpochMuxes {
        use commonware_cryptography::{bls12381, Signer as _};
        use commonware_p2p::{
            simulated::{Config as SimConfig, Network as SimNetwork},
            utils::mux::Muxer,
        };
        use commonware_runtime::Quota;
        use commonware_utils::{NZUsize, NZU32};
        use std::num::NonZeroUsize;

        let (network, oracle) = SimNetwork::new(
            ctx.child("network"),
            SimConfig {
                max_size: 64 * 1024,
                disconnect_on_block: true,
                max_peers_per_set: NonZeroUsize::new(1).unwrap(),
                tracked_peer_sets: NZUsize!(1),
            },
        );
        network.start();

        let pk = bls12381::PrivateKey::from_seed(7).public_key();
        let control = oracle.control(pk);
        let quota = Quota::per_second(NZU32!(1_000));
        let (votes_tx, votes_rx) = control.register(0, quota).await.unwrap();
        let (certs_tx, certs_rx) = control.register(1, quota).await.unwrap();
        let (res_tx, res_rx) = control.register(2, quota).await.unwrap();

        let (vote_muxer, vote) = Muxer::new(ctx.child("vote_mux"), votes_tx, votes_rx);
        vote_muxer.start();
        let (cert_muxer, cert) = Muxer::new(ctx.child("cert_mux"), certs_tx, certs_rx);
        let (cert_handle, _held_cert_muxer) = if start_cert_muxer {
            (Some(cert_muxer.start()), None)
        } else {
            (None, Some(cert_muxer))
        };
        let (res_muxer, res) = Muxer::new(ctx.child("res_mux"), res_tx, res_rx);
        res_muxer.start();

        TestEpochMuxes {
            vote,
            cert,
            res,
            cert_handle,
            _held_cert_muxer,
        }
    }

    #[test]
    fn poll_mux_registration_cancels_partial_epoch_subchannels_on_stop() {
        use super::poll_mux_registration_until_stopped;
        use commonware_consensus::types::Epoch;

        deterministic::Runner::default().start(|ctx| async move {
            let mut muxes = setup_epoch_muxes(&ctx, false).await;
            let mut vote_probe = muxes.vote.clone();

            let reg = poll_mux_registration_until_stopped(
                &ctx,
                crate::epoch_subchannels::register_epoch_subchannels(
                    Epoch::new(7),
                    &mut muxes.vote,
                    &mut muxes.cert,
                    &mut muxes.res,
                ),
            );
            futures::pin_mut!(reg);

            // First poll enqueues vote registration on the FIFO vote muxer.
            assert!(reg.as_mut().now_or_never().is_none());
            // A later registration on the same FIFO muxer guarantees vote(7) completed.
            let _fence = vote_probe.register(999).await.unwrap();
            // Second poll stores vote(7) inside the future and parks on unstarted cert_mux.
            assert!(reg.as_mut().now_or_never().is_none());
            // Public mux state confirms vote(7) is registered while cert(7) is still pending.
            assert!(matches!(
                vote_probe.register(7).await,
                Err(MuxError::AlreadyRegistered(7))
            ));

            let stop = ctx.child("mid_reg_stop").stop(0, None);
            futures::pin_mut!(stop);
            let stop_result = stop.as_mut().now_or_never();

            let channels = reg
                .await
                .expect("stop during partial multi-channel registration must return Ok(None)");
            assert!(channels.is_none());
            match stop_result {
                Some(result) => result.unwrap(),
                None => stop.await.unwrap(),
            }
        });
    }

    #[test]
    fn poll_mux_registration_propagates_live_epoch_subchannel_errors() {
        use super::poll_mux_registration_until_stopped;
        use commonware_consensus::types::Epoch;
        use outbe_simplex::errors::EpochSubchannelError;

        deterministic::Runner::default().start(|ctx| async move {
            let mut muxes = setup_epoch_muxes(&ctx, true).await;

            // Live occupied subchannel fails with EpochSubchannelError::Register.
            let _occupied = muxes.vote.register(1).await.unwrap();
            let dup_err = match poll_mux_registration_until_stopped(
                &ctx,
                crate::epoch_subchannels::register_epoch_subchannels(
                    Epoch::new(1),
                    &mut muxes.vote,
                    &mut muxes.cert,
                    &mut muxes.res,
                ),
            )
            .await
            {
                Ok(_) => panic!("occupied vote subchannel must fail while live"),
                Err(err) => err,
            };
            assert!(matches!(
                dup_err.downcast_ref::<EpochSubchannelError>(),
                Some(EpochSubchannelError::Register { .. })
            ));

            // Live closed mux fails with EpochSubchannelError::Register.
            let cert_handle = muxes.cert_handle.take().expect("cert muxer started");
            cert_handle.abort();
            let _ = cert_handle.await;
            let closed_err = match poll_mux_registration_until_stopped(
                &ctx,
                crate::epoch_subchannels::register_epoch_subchannels(
                    Epoch::new(2),
                    &mut muxes.vote,
                    &mut muxes.cert,
                    &mut muxes.res,
                ),
            )
            .await
            {
                Ok(_) => panic!("closed cert mux must fail while live"),
                Err(err) => err,
            };
            assert!(matches!(
                closed_err.downcast_ref::<EpochSubchannelError>(),
                Some(EpochSubchannelError::Register { .. })
            ));
        });
    }

    #[test]
    fn supervise_global_stop_drains_clean_engine_and_surfaces_drain_failures() {
        use super::{supervise_epoch_loop_result, EpochLoopAction, EpochLoopOutcome};
        use crate::application_shutdown::ApplicationDrain;
        use crate::errors::StackShutdownError;

        // Clean engine drain within 5s returns ExitStack.
        deterministic::Runner::default().start(|ctx| async move {
            let mut engine = ctx.child("clean_engine").spawn(|_| async {});
            let action = supervise_epoch_loop_result(
                &ctx,
                Ok(EpochLoopOutcome::GlobalStop),
                &mut engine,
                &ApplicationDrain::default(),
            )
            .await
            .expect("clean engine drain must succeed");
            assert_eq!(action, EpochLoopAction::ExitStack);
        });

        // Stalled engine past 5s returns SimplexEngineDrainTimedOut.
        deterministic::Runner::default().start(|ctx| async move {
            let mut engine = ctx
                .child("stalled_engine")
                .spawn(|_| std::future::pending::<()>());
            let err = supervise_epoch_loop_result(
                &ctx,
                Ok(EpochLoopOutcome::GlobalStop),
                &mut engine,
                &ApplicationDrain::default(),
            )
            .await
            .expect_err("stalled engine drain must time out");
            assert!(matches!(
                err.downcast_ref::<StackShutdownError>(),
                Some(StackShutdownError::SimplexEngineDrainTimedOut(_))
            ));
        });

        // Aborted engine handle returns SimplexEngineFailedWhileDraining.
        deterministic::Runner::default().start(|ctx| async move {
            let mut engine = ctx
                .child("aborted_engine")
                .spawn(|_| std::future::pending::<()>());
            engine.abort();
            let err = supervise_epoch_loop_result(
                &ctx,
                Ok(EpochLoopOutcome::GlobalStop),
                &mut engine,
                &ApplicationDrain::default(),
            )
            .await
            .expect_err("aborted engine drain must fail");
            assert!(matches!(
                err.downcast_ref::<StackShutdownError>(),
                Some(StackShutdownError::SimplexEngineFailedWhileDraining(_))
            ));
        });
    }
}
