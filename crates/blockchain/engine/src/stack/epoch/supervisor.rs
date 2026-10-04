//! Preserve select ordering while delegating domain events to epoch operations.
use super::super::*;
use super::runtime::*;

impl<E> EpochSupervisor<E>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    pub(super) async fn run(
        mut self,
        ctx: E,
        recovered_pending_boundary: Option<PendingDkgBoundarySnapshot>,
        recovery_anchor_height: u64,
    ) -> Result<()> {
        self.restore_pending(recovered_pending_boundary, recovery_anchor_height)
            .await?;
        let mut latest_consensus_tip = *self.consensus_tip_rx.borrow();
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
            min_block_time = ?self.bt.min_block_time,
            leader_timeout = ?self.bt.leader_timeout,
            certification_timeout = ?self.bt.certification_timeout,
            "consensus timeouts (genesis-sourced, no CLI override)"
        );
        'epoch_loop: loop {
            // -- a. Register or take pre-registered epoch sub-channels -------
            // Activation pre-registers `next_epoch_subchannels` at DKG
            // completion (see DKG completion handler below); the top of the
            // next iteration consumes it. The fallback path covers the
            // genesis-bootstrap iteration where no prior DKG completion has
            // run.
            let current_subchannels = if self.channels.replacement_epoch_subchannels.is_some() {
                outbe_consensus::epoch_subchannels::take_or_register_current(
                    self.state.current_epoch,
                    &mut self.channels.replacement_epoch_subchannels,
                    &mut self.channels.vote_mux,
                    &mut self.channels.cert_mux,
                    &mut self.channels.res_mux,
                )
                .await?
            } else {
                outbe_consensus::epoch_subchannels::take_or_register_current(
                    self.state.current_epoch,
                    &mut self.channels.next_epoch_subchannels,
                    &mut self.channels.vote_mux,
                    &mut self.channels.cert_mux,
                    &mut self.channels.res_mux,
                )
                .await?
            };
            if let Some(pending_epoch) = self.rotation.deferred_startup_pending_epoch.take() {
                ensure!(
                    self.channels.next_epoch_subchannels.is_none(),
                    "recovered future DKG handoff collided with an existing subchannel stash"
                );
                self.channels.next_epoch_subchannels = Some(
                outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                    pending_epoch,
                    &mut self.channels.vote_mux,
                    &mut self.channels.cert_mux,
                    &mut self.channels.res_mux,
                )
                .await
                .wrap_err_with(|| {
                    format!(
                        "pre-register recovered future-epoch subchannels after acquiring current epoch {current_epoch}"
                    ,current_epoch = self.state.current_epoch)
                })?,
            );
                info!(
                    active_epoch = %self.state.current_epoch,
                    pending_epoch = %pending_epoch,
                    "pre-registered recovered future DKG channels after current epoch"
                );
            }

            let (mut engine_handle_task, radicle_signer) =
                self.start_engine(&ctx, current_subchannels).await?;
            // -- g. Engine event loop ----------------------------------------
            // Monitors engine, component exits, and block-height-driven reshare triggers.

            let epoch_loop_result: Result<EpochLoopOutcome> = async {
            let mut stack_shutdown = ctx.stopped();

            loop {
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

                desired_signer = wait_for_radicle_role_change(
                    &mut self.radicle_updates,
                    radicle_signer,
                    self.state.signing_share.is_some(),
                ) => {
                    let desired_signer = desired_signer?;
                    info!(
                        epoch = %self.state.current_epoch,
                        previous_signer = radicle_signer,
                        desired_signer,
                        "canonical Radicle voting gate changed; replacing same-epoch Simplex role"
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
                    if let EventAction::Outcome(outcome) = self.complete_dkg(dkg_result).await? { return Ok(outcome); }
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

                _ = &mut provider_ready_retry_timer => {
                    provider_ready_retry_timer = Box::pin(std::future::pending());
                    if let Some(current_height) = pending_provider_ready_height {
                        let _ = self.execution_finalized_height_tx.send(current_height);
                    }
                },

                // Block-height based DKG/VRF rotation. This is driven by execution-finalized
                // height notifications after successful new_payload + FCU, not wall-clock
                // polling or raw consensus finalization.
                Some(current_height) = wait_for_execution_finalized_height => {
                    match latest_consensus_tip {
                        Some(tip) => {
                            if !provider_matches_consensus_tip(&self.node.provider, tip, current_height)? {
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

                    match self.activate_pending(&ctx, current_height, &mut engine_handle_task).await? {
                        EventAction::Outcome(outcome) => return Ok(outcome),
                        EventAction::Continue => continue,
                        EventAction::Proceed => {}
                    }
                    if let EventAction::Outcome(outcome) = self.schedule_rotation(&ctx, current_height).await? { return Ok(outcome); }
                },

                // Component exits -> fatal.
                result = &mut self.actors.executor_handle_task => {
                    info!("executor actor exited");
                    let executor_result = result
                        .map_err(|e| eyre::eyre!("executor actor task failed: {e:?}"))?;
                    executor_result.wrap_err("executor actor returned fatal error")?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                result = &mut self.actors.handler_handle => {
                    info!("application handler exited");
                    let application_result = result
                        .map_err(|e| eyre::eyre!("application handler task failed: {e:?}"))?;
                    application_result.wrap_err("application handler returned fatal error")?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                result = &mut self.actors.finalization_handle => {
                    info!("finalization actor exited");
                    let finalization_result = result
                        .map_err(|e| eyre::eyre!("finalization actor task failed: {e:?}"))?;
                    finalization_result.wrap_err("finalization actor returned fatal error")?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                result = &mut self.actors.peer_manager_handle_task => {
                    info!("peer manager actor exited");
                    result.map_err(|e| eyre::eyre!("peer manager actor exited: {e:?}"))?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                // SSA-8: the marshal actor is consensus-liveness-critical (block
                // availability, finalized-block delivery to the executor). With
                // `catch_panics`, a marshal panic (e.g. an unacknowledged Exact,
                // or a future telemetry-label assert) resolves its handle instead
                // of aborting the process - so an UNmonitored handle would leave
                // the node silently stalled (no blocks delivered, consensus
                // wedged). Monitor it like the other components: a marshal exit
                // is fatal and shuts the node down with the cause.
                result = &mut self.actors.marshal_handle => {
                    info!("marshal actor exited");
                    result.map_err(|e| eyre::eyre!("marshal actor exited: {e:?}"))?;
                    return Ok(EpochLoopOutcome::StackExit);
                },
                // The broadcast (buffered dissemination) handle remains managed by
                // the Commonware runtime; its failure degrades to the marshal
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
                    self.channels.replacement_epoch_subchannels = Some(
                    outbe_consensus::epoch_subchannels::reacquire_epoch_subchannels_with_policy(
                        self.state.current_epoch,
                        &ctx,
                        outbe_consensus::epoch_subchannels::SubchannelRetryPolicy {
                            timeout: Duration::from_secs(5),
                            retry_interval: Duration::from_millis(10),
                        },
                        outbe_consensus::epoch_subchannels::EpochMuxHandles {
                            vote: &mut self.channels.vote_mux,
                            cert: &mut self.channels.cert_mux,
                            res: &mut self.channels.res_mux,
                        },
                    )
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "reacquire same-epoch channels while replacing Radicle role in epoch {current_epoch}"
                        ,current_epoch = self.state.current_epoch)
                    })?,
                );
                    continue 'epoch_loop;
                }
                EpochLoopAction::ExitStack => break 'epoch_loop,
            }
        }

        Ok(())
    }

    pub(super) async fn promote_boundary(
        &mut self,
        scope: RetireScope,
    ) -> Result<BoundaryPromotion> {
        let finalized_height = self.finalization_view.read().last_finalized_number;
        adopt_finalized_boundary_carrier(
            &self.dkg_manager,
            &self.node.provider,
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
