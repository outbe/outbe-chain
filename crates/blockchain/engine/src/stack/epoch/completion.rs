//! Completed DKG ceremonies become durable preannounce candidates.
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
    pub(super) async fn complete_dkg(
        &mut self,
        dkg_result: Result<DkgTaskOutcome>,
    ) -> Result<EventAction> {
        self.rotation.reshare_in_progress = false;
        match dkg_result {
            Ok(DkgTaskOutcome::Complete(dkg_complete)) => {
                let Some(target) = self.rotation.frozen_dkg_target.take() else {
                    warn!("DKG completed without a frozen target; ignoring stale outcome");
                    outbe_consensus::metrics::record_dkg_status(0);
                    return Ok(EventAction::Continue);
                };

                let current_height = self.node.provider.last_block_number().map_err(|error| {
                    eyre::eyre!("failed to read latest block height after DKG completion: {error}")
                })?;
                if frozen_dkg_target_expired(
                    current_height,
                    target.planned_activation_height,
                    self.dkg_rotation_params.activation_grace_blocks,
                ) {
                    let deadline = target
                        .planned_activation_height
                        .saturating_add(self.dkg_rotation_params.activation_grace_blocks);
                    self.vrf_safety.mark_expired(current_height);
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    return Err(eyre::eyre!(
                                    "DKG completed without time for an outgoing-finalized preannounce: cycle {}, height {}, deadline {}",
                                    target.dkg_cycle,
                                    current_height,
                                    deadline
                                ));
                }

                // Recording the next boundary drops an untaken commit.
                if self
                    .promote_boundary(RetireScope::PendingMaterialOnly)
                    .await?
                    == BoundaryPromotion::LocalExcluded
                {
                    return Ok(EventAction::Outcome(EpochLoopOutcome::StackExit));
                }
                let boundary_artifact = if let Some(ref keys_dir) = self.args.keys_dir {
                    persist_completed_dkg_before_activation(
                        keys_dir,
                        &self.key_backend,
                        self.state.current_epoch,
                        self.state.vrf_material_version,
                        &self.state.participants,
                        &target,
                        &dkg_complete,
                        current_height,
                    )?
                } else {
                    build_completed_dkg_boundary(
                        self.state.current_epoch,
                        self.state.vrf_material_version,
                        &self.state.participants,
                        &target,
                        &dkg_complete.output,
                        &dkg_complete.participants,
                    )?
                };

                // Publish only after the exact artifact and threshold
                // material are durable. Proposers can now carry this
                // next-epoch artifact as a CommitteePreAnnounce before
                // activation; the same immutable object is retained for
                // the activation boundary below.
                self.dkg_manager
                    .note_ceremony_completed(boundary_artifact.clone());

                info!(
                    epoch = %self.state.current_epoch,
                    dkg_cycle = target.dkg_cycle,
                    is_validator_set_change = target.is_validator_set_change,
                    planned_activation_height = target.planned_activation_height,
                    current_height,
                    "DKG completed; waiting for exact outgoing-finalized preannounce carrier"
                );
                outbe_consensus::metrics::record_dkg_status(2); // completed
                outbe_consensus::metrics::record_reshare_completed();
                if current_height > target.planned_activation_height {
                    self.vrf_safety.note_grace(
                        target.planned_activation_height,
                        self.dkg_rotation_params.activation_grace_blocks,
                    );
                } else {
                    self.vrf_safety.note_pending_activation(
                        target.planned_activation_height,
                        self.dkg_rotation_params.activation_grace_blocks,
                    );
                }
                publish_randomness_status(&self.bridge, &self.vrf_safety);

                // Pre-register vote/cert/res sub-channels for
                // the upcoming epoch BEFORE stashing the pending
                // activation and BEFORE the
                // `execution_finalized_height_tx.send(...)`
                // call that may immediately wake the activation
                // branch (when `should_activate_now` is true).
                // This closes the cross-node race where a
                // faster peer can begin broadcasting epoch-N+1
                // traffic before this node has registered the
                // matching sub-channel on its Mux. See
                // `epoch_subchannels::register_epoch_subchannels`.
                //
                // Fail-fast: a Mux-level error
                // (AlreadyRegistered, closed Mux) on a
                // consensus-critical channel is a hard fault.
                // No silent fallback to lazy registration.
                let next_epoch =
                    next_consensus_epoch_after_dkg_activation(self.state.current_epoch);
                if self.channels.next_epoch_subchannels.is_some() {
                    warn!(
                        epoch = %next_epoch,
                        "stale DKG completion arrived after next-epoch subchannels were already pre-registered; ignoring"
                    );
                    return Ok(EventAction::Continue);
                }
                self.channels.next_epoch_subchannels = Some(
                    outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                        next_epoch,
                        &mut self.channels.vote_mux,
                        &mut self.channels.cert_mux,
                        &mut self.channels.res_mux,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "pre-register next-epoch subchannels at DKG \
                                         completion for epoch {next_epoch}"
                        )
                    })?,
                );

                self.rotation.pending_dkg_activation = Some(PendingDkgActivation {
                    target,
                    complete: dkg_complete,
                    boundary_artifact,
                    recovered_output: None,
                });
                let _ = self.execution_finalized_height_tx.send(current_height);
            }
            Ok(DkgTaskOutcome::DealerOnly(dealer_only_complete)) => {
                let Some(target) = self.rotation.frozen_dkg_target.as_ref().cloned() else {
                    warn!(
                        "dealer-only DKG completed without a frozen target; ignoring stale outcome"
                    );
                    outbe_consensus::metrics::record_dkg_status(0);
                    return Ok(EventAction::Continue);
                };
                if dealer_only_complete.participants != target.participants {
                    return Err(eyre::eyre!(
                        "dealer-only DKG participant set does not match frozen target"
                    ));
                }

                let current_height =
                                self.node.provider.last_block_number().map_err(|error| {
                                    eyre::eyre!(
                                        "failed to read latest block height after dealer-only DKG completion: {error}"
                                    )
                                })?;
                if frozen_dkg_target_expired(
                    current_height,
                    target.planned_activation_height,
                    self.dkg_rotation_params.activation_grace_blocks,
                ) {
                    let deadline = target
                        .planned_activation_height
                        .saturating_add(self.dkg_rotation_params.activation_grace_blocks);
                    self.vrf_safety.mark_expired(current_height);
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    return Err(eyre::eyre!(
                                    "dealer-only DKG completed without time for an outgoing-finalized preannounce: cycle {}, height {}, deadline {}",
                                    target.dkg_cycle,
                                    current_height,
                                    deadline
                                ));
                }
                info!(
                    epoch = %self.state.current_epoch,
                    dkg_cycle = target.dkg_cycle,
                    planned_activation_height = target.planned_activation_height,
                    current_height,
                    "dealer-only DKG completed; remaining in old validator set until activation"
                );
                self.rotation.dealer_only_dkg_activation = Some(DealerOnlyDkgActivation {
                    target,
                    boundary_artifact: None,
                    recovered_output: None,
                });
                outbe_consensus::metrics::record_dkg_status(2);
                outbe_consensus::metrics::record_reshare_completed();
                let _ = self.execution_finalized_height_tx.send(current_height);
            }
            Err(e) => {
                // Height notifications are deliberately not consumed while a
                // ceremony is running. Check the authoritative finalized view
                // before scheduling a retry: otherwise an old queued height can
                // start another ceremony while the chain is already at the VRF
                // deadline, and the application cannot propose the next block
                // that would wake this branch again.
                if let Some(target) = self.rotation.frozen_dkg_target.as_ref() {
                    let current_height = self.finalization_view.read().last_finalized_number;
                    if frozen_dkg_target_expired(
                        current_height,
                        target.planned_activation_height,
                        self.dkg_rotation_params.activation_grace_blocks,
                    ) {
                        let activation_deadline = target
                            .planned_activation_height
                            .saturating_add(self.dkg_rotation_params.activation_grace_blocks);
                        self.vrf_safety.mark_expired(current_height);
                        publish_randomness_status(&self.bridge, &self.vrf_safety);
                        return Err(eyre::eyre!(
                            "frozen DKG target missed VRF expiry: cycle {}, height {}, deadline {}",
                            target.dkg_cycle,
                            current_height,
                            activation_deadline
                        ));
                    }
                }
                warn!(
                    ?e,
                    "DKG reshare failed, retrying frozen target on next check"
                );
                self.rotation.retry_frozen_dkg = true;
            }
        }

        Ok(EventAction::Proceed)
    }
}
