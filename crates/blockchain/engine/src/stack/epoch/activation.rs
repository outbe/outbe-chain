//! Activate only an exact outgoing-finalized DKG handoff.
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
    pub(super) async fn activate_pending(
        &mut self,
        ctx: &E,
        current_height: u64,
        engine: &mut commonware_runtime::Handle<()>,
    ) -> Result<EventAction> {
        if let Some(pending) = self.rotation.pending_dkg_activation.as_ref() {
            let consensus_finalized_height = self
                .finalization_view
                .read()
                .last_finalized_number
                .min(current_height);
            let exact_carrier_height = find_exact_finalized_preannounce_carrier(
                &self.node.provider,
                &pending.boundary_artifact,
                consensus_finalized_height,
                self.dkg_rotation_params.activation_grace_blocks,
            )?;
            match pending_dkg_handoff_decision(
                consensus_finalized_height,
                pending.target.planned_activation_height,
                self.dkg_rotation_params.activation_grace_blocks,
                exact_carrier_height,
            ) {
                PendingDkgHandoffDecision::Expired { deadline } => {
                    self.vrf_safety.mark_expired(consensus_finalized_height);
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    return Err(eyre::eyre!(
                                    "pending DKG activation missed VRF expiry: cycle {}, height {}, deadline {}",
                                    pending.target.dkg_cycle,
                                    consensus_finalized_height,
                                    deadline
                                ));
                }
                PendingDkgHandoffDecision::Wait => {}
                PendingDkgHandoffDecision::Activate {
                    activation_anchor: activation_height,
                } => {
                    let Some(canonical_output) = select_pending_canonical_output(
                        self.dkg_manager.canonical_output(self.state.current_epoch),
                        pending.recovered_output.as_ref(),
                    ) else {
                        warn!(
                            epoch = %self.state.current_epoch,
                            activation_height,
                            "DKG activation height reached but canonical finalized-log output is not ready"
                        );
                        return Ok(EventAction::Continue);
                    };
                    let Some(pending) = self.rotation.pending_dkg_activation.take() else {
                        return Err(eyre::eyre!(
                                        "pending DKG activation missing after precheck at height {activation_height}"
                                    ));
                    };
                    let target = pending.target;
                    let dkg_complete = pending.complete;
                    let boundary_artifact = pending.boundary_artifact;
                    if let Err(error) = dkg_manager::assert_canonical_output(
                        &dkg_complete.output,
                        &canonical_output,
                        &format!("cycle {}", target.dkg_cycle),
                    ) {
                        self.vrf_safety.mark_expired(activation_height);
                        publish_randomness_status(&self.bridge, &self.vrf_safety);
                        return Err(error);
                    }

                    let activated_validator_set = validator_set_for_dkg_output_players(
                        &canonical_output,
                        &target.validator_set,
                    )?;
                    let activated_participants =
                        participants_from_validator_set(&activated_validator_set)?;
                    let activated_is_validator_set_change =
                        activated_participants != self.state.participants;
                    // invariant:
                    // `vrf_material_version` increments by exactly 1 per
                    // successful reshare activation. Overflow is a
                    // deterministic activation error, not saturation.
                    // The single source of truth lives in the
                    // `outbe-validatorset` crate so proposer and
                    // validator paths cannot diverge.
                    let activated_vrf_material_version =
                        outbe_validatorset::next_vrf_material_version(
                            self.state.vrf_material_version,
                        )?;
                    let activated_polynomial = canonical_output.public().clone();
                    let activated_signing_share = Some(dkg_complete.share);
                    let next_epoch =
                        next_consensus_epoch_after_dkg_activation(self.state.current_epoch);
                    ensure!(
                        boundary_artifact.epoch == next_epoch.get(),
                        "completed DKG boundary epoch {} does not match activation epoch {}",
                        boundary_artifact.epoch,
                        next_epoch.get()
                    );
                    let published_boundary = self
                        .dkg_manager
                        .pending_boundary_artifact(next_epoch)
                        .await
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "completed DKG boundary is missing from manager at activation"
                            )
                        })?;
                    ensure!(
                        published_boundary == boundary_artifact,
                        "published pre-announce boundary diverged before activation"
                    );
                    let epoch_boundary_height =
                        activation_height.max(target.planned_activation_height);

                    self.state.vrf_material_version = activated_vrf_material_version;
                    self.state.polynomial = activated_polynomial;
                    self.state.last_dkg_output = Some(canonical_output.clone());
                    self.state.signing_share = activated_signing_share;
                    activate_vrf_material_and_publish_local_share(
                        &self.bridge,
                        &self.vrf_materials,
                        self.state.vrf_material_version,
                        self.state.polynomial.clone(),
                        self.state.signing_share.clone(),
                    );

                    self.application_epoch_fence
                        .arm_activation_boundary(self.state.current_epoch, epoch_boundary_height);
                    debug!(
                        epoch = %self.state.current_epoch,
                        dkg_cycle = target.dkg_cycle,
                        max_block_height = epoch_boundary_height,
                        "armed application epoch fence for DKG activation"
                    );

                    self.state.last_dkg_activation_height =
                        activation_height.max(target.planned_activation_height);
                    self.vrf_safety.note_activated(
                        self.state.vrf_material_version,
                        self.state.last_dkg_activation_height,
                        self.dkg_rotation_params
                            .planned_activation_height(self.state.last_dkg_activation_height),
                        self.dkg_rotation_params.activation_grace_blocks,
                    );
                    info!(
                    target: "outbe_engine::stack",
                                                   dkg_cycle = target.dkg_cycle,
                                                   activation_height = self.state.last_dkg_activation_height,
                                                   planned_activation_height = target.planned_activation_height,
                                                   vrf_material_version = self.state.vrf_material_version,
                                                   vrf_group_public_key = %vrf_group_public_key_hash(&self.state.polynomial),
                                                   dkg_output_hash = %dkg_manager::dkg_output_hash(&canonical_output),
                                                   dkg_public_polynomial_hash = %dkg_manager::public_polynomial_hash(&self.state.polynomial),
                                                   is_validator_set_change = activated_is_validator_set_change,
                                                   "VRF/DKG material activated"
                                               );
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    register_epoch_validation_providers(
                        EpochValidationCommittee {
                            epoch: next_epoch,
                            participants: &activated_participants,
                            validator_set: &activated_validator_set,
                            recovered_boundary: None,
                        },
                        EpochValidationProviders {
                            vrf_materials: &self.vrf_materials,
                            certificate_scheme: &self.certificate_scheme_provider,
                            committee: &self.committee_provider,
                        },
                    )?;
                    self.state.validator_set = activated_validator_set;
                    self.rotation.frozen_dkg_target = None;
                    outbe_consensus::metrics::record_dkg_status(0);

                    self.state.participants = activated_participants;
                    self.application_epoch_fence.advance_epoch(next_epoch);
                    engine.abort();
                    self.state.current_epoch = next_epoch;
                    info!(
                        epoch = %self.state.current_epoch,
                        vrf_material_version = self.state.vrf_material_version,
                        is_validator_set_change = activated_is_validator_set_change,
                        "DKG activation advanced consensus epoch; restarting Simplex engine"
                    );
                    // DKG activation race: before bouncing back
                    // into the epoch loop (which will call `engine.start`
                    // for the new epoch), wait for the FinalizationActor
                    // to publish the activation block as finalized. The
                    // generic `current_epoch > 0` guard at the top of
                    // the loop checks only that *some* finalized anchor
                    // exists, which is a weaker condition than
                    // `last_finalized_number >= activation_height` - a
                    // stale anchor would still satisfy the generic
                    // guard while pointing Simplex at the wrong parent.
                    let activation_height = self.state.last_dkg_activation_height;
                    super::continuity::wait_for_activation_anchor(
                        ctx,
                        &self.finalization_view,
                        activation_height,
                        super::continuity::AnchorTransition::Dkg,
                    )
                    .await?;
                    return Ok(EventAction::Outcome(EpochLoopOutcome::RestartEpoch));
                }
            }
        }

        if self
            .rotation
            .dealer_only_dkg_activation
            .as_ref()
            .is_some_and(|pending| pending.boundary_artifact.is_none())
        {
            if let Some(canonical_output) =
                self.dkg_manager.canonical_output(self.state.current_epoch)
            {
                let target = self
                    .rotation
                    .dealer_only_dkg_activation
                    .as_ref()
                    .map(|pending| pending.target.clone())
                    .ok_or_else(|| {
                        eyre::eyre!("dealer-only activation disappeared while preparing boundary")
                    })?;
                // Recording the next boundary drops an untaken commit.
                if self
                    .promote_boundary(RetireScope::PendingMaterialOnly)
                    .await?
                    == BoundaryPromotion::LocalExcluded
                {
                    return Ok(EventAction::Outcome(EpochLoopOutcome::StackExit));
                }
                let boundary_artifact = if let Some(ref keys_dir) = self.args.keys_dir {
                    persist_observed_dkg_boundary_before_activation(
                        keys_dir,
                        DkgBoundaryContext {
                            current_epoch: self.state.current_epoch,
                            vrf_material_version: self.state.vrf_material_version,
                            current_participants: &self.state.participants,
                            target: &target,
                        },
                        &canonical_output,
                        current_height,
                    )?
                } else {
                    build_completed_dkg_boundary(
                        DkgBoundaryContext {
                            current_epoch: self.state.current_epoch,
                            vrf_material_version: self.state.vrf_material_version,
                            current_participants: &self.state.participants,
                            target: &target,
                        },
                        &canonical_output,
                        &target.participants,
                    )?
                };
                self.dkg_manager
                    .note_ceremony_completed(boundary_artifact.clone());
                if self.channels.next_epoch_subchannels.is_none() {
                    let next_epoch =
                        next_consensus_epoch_after_dkg_activation(self.state.current_epoch);
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
                                            "pre-register next-epoch subchannels for dealer-only handoff epoch {next_epoch}"
                                        )
                                    })?,
                                );
                }
                let pending = self
                    .rotation
                    .dealer_only_dkg_activation
                    .as_mut()
                    .ok_or_else(|| {
                        eyre::eyre!(
                            "dealer-only activation disappeared before boundary publication"
                        )
                    })?;
                pending.boundary_artifact = Some(boundary_artifact);
                info!(
                    epoch = %self.state.current_epoch,
                    dkg_cycle = target.dkg_cycle,
                    "dealer-only DKG reconstructed and published durable pending boundary"
                );
            }
        }

        let dealer_only_decision =
            self.rotation
                .dealer_only_dkg_activation
                .as_ref()
                .and_then(|d| {
                    d.boundary_artifact.as_ref().map(|boundary_artifact| {
                        (
                            d.target.planned_activation_height,
                            d.target.dkg_cycle,
                            boundary_artifact.clone(),
                        )
                    })
                });
        if let Some((planned_activation_height, target_dkg_cycle, boundary_artifact)) =
            dealer_only_decision
        {
            let consensus_finalized_height = self
                .finalization_view
                .read()
                .last_finalized_number
                .min(current_height);
            let exact_carrier_height = find_exact_finalized_preannounce_carrier(
                &self.node.provider,
                &boundary_artifact,
                consensus_finalized_height,
                self.dkg_rotation_params.activation_grace_blocks,
            )?;
            match pending_dkg_handoff_decision(
                consensus_finalized_height,
                planned_activation_height,
                self.dkg_rotation_params.activation_grace_blocks,
                exact_carrier_height,
            ) {
                PendingDkgHandoffDecision::Expired { deadline } => {
                    self.vrf_safety.mark_expired(consensus_finalized_height);
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    return Err(eyre::eyre!(
                                    "dealer-only DKG activation missed VRF expiry: cycle {}, height {}, deadline {}",
                                    target_dkg_cycle,
                                    consensus_finalized_height,
                                    deadline
                                ));
                }
                PendingDkgHandoffDecision::Wait => {}
                PendingDkgHandoffDecision::Activate {
                    activation_anchor: activation_height,
                } => {
                    // S3 demotion: an exited validator (deactivated/unstaked) is a
                    // previous-output dealer but not a frozen-target player, so it
                    // finishes its dealer duties for the resharded committee and then,
                    // instead of looping until VRF expiry kills the process, DEMOTES to
                    // a share-less verifier-follower of the smaller (N-1) committee. It
                    // adopts the new group polynomial reconstructed from the finalized
                    // dealer logs it just helped produce (`canonical_output` for the
                    // ceremony epoch), drops its share, advances its epoch, and restarts
                    // the Simplex engine in verifier mode - the same finalized-follower
                    // path a non-staked TEE full-node uses. The reshared output is a
                    // membership change, so unlike the same-membership verifier-follow
                    // the node MUST take the new polynomial + participant set here (it
                    // has them from the ceremony) rather than reusing the old ones.
                    let canonical_output = select_pending_canonical_output(
                                    self.dkg_manager.canonical_output(self.state.current_epoch),
                                    self.rotation.dealer_only_dkg_activation
                                        .as_ref()
                                        .and_then(|pending| pending.recovered_output.as_ref()),
                                )
                                    .ok_or_else(|| eyre::eyre!(
                                        "dealer-only pending boundary lost its canonical output before activation"
                                    ))?;
                    let next_epoch =
                        next_consensus_epoch_after_dkg_activation(self.state.current_epoch);
                    let published_boundary = self.dkg_manager
                                    .pending_boundary_artifact(next_epoch)
                                    .await
                                    .ok_or_else(|| {
                                        eyre::eyre!(
                                            "dealer-only completed DKG boundary is missing from manager at activation"
                                        )
                                    })?;
                    ensure!(
                        published_boundary == boundary_artifact,
                        "dealer-only published preannounce boundary diverged before activation"
                    );
                    let Some(dealer_only) = self.rotation.dealer_only_dkg_activation.take() else {
                        return Err(eyre::eyre!(
                                        "dealer-only activation missing after decision at height {activation_height}"
                                    ));
                    };
                    let target = dealer_only.target;
                    let activated_validator_set = validator_set_for_dkg_output_players(
                        &canonical_output,
                        &target.validator_set,
                    )?;
                    let activated_participants =
                        participants_from_validator_set(&activated_validator_set)?;
                    info!(
                        epoch = %self.state.current_epoch,
                        next_epoch = %next_epoch,
                        activation_height,
                        old = self.state.participants.len(),
                        new = activated_participants.len(),
                        "shareless verifier: authenticated DKG handoff complete"
                    );
                    let new_vrf_material_version =
                        match outbe_validatorset::next_vrf_material_version(
                            self.state.vrf_material_version,
                        ) {
                            Ok(version) => version,
                            Err(error) => {
                                warn!(%error, "exited validator: vrf material version overflow during demotion; reusing current");
                                self.state.vrf_material_version
                            }
                        };
                    self.state.signing_share = None;
                    self.state.polynomial = canonical_output.public().clone();
                    self.state.last_dkg_output = Some(canonical_output);
                    self.state.validator_set = activated_validator_set;
                    self.state.participants = activated_participants;
                    self.state.vrf_material_version = new_vrf_material_version;
                    self.state.dkg_cycle = target.dkg_cycle.saturating_add(1);
                    activate_vrf_material_and_publish_local_share(
                        &self.bridge,
                        &self.vrf_materials,
                        self.state.vrf_material_version,
                        self.state.polynomial.clone(),
                        None,
                    );
                    register_epoch_validation_providers(
                        EpochValidationCommittee {
                            epoch: next_epoch,
                            participants: &self.state.participants,
                            validator_set: &self.state.validator_set,
                            recovered_boundary: None,
                        },
                        EpochValidationProviders {
                            vrf_materials: &self.vrf_materials,
                            certificate_scheme: &self.certificate_scheme_provider,
                            committee: &self.committee_provider,
                        },
                    )?;
                    let anchored_height = activation_height;
                    self.state.last_dkg_activation_height = activation_height;
                    self.vrf_safety.note_activated(
                        self.state.vrf_material_version,
                        anchored_height,
                        self.dkg_rotation_params
                            .planned_activation_height(anchored_height),
                        self.dkg_rotation_params.activation_grace_blocks,
                    );
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    self.application_epoch_fence
                        .arm_activation_boundary(self.state.current_epoch, activation_height);
                    self.rotation.frozen_dkg_target = None;
                    self.application_epoch_fence.advance_epoch(next_epoch);
                    engine.abort();
                    self.state.current_epoch = next_epoch;
                    // Anchor wait (mirror the verifier activation): the restarted
                    // verifier engine's floor needs the activation block finalized
                    // before `'epoch_loop` rebuilds the verifier scheme.
                    super::continuity::wait_for_activation_anchor(
                        ctx,
                        &self.finalization_view,
                        activation_height,
                        super::continuity::AnchorTransition::DealerDemotion,
                    )
                    .await?;
                    return Ok(EventAction::Outcome(EpochLoopOutcome::RestartEpoch));
                }
            }
        }

        Ok(EventAction::Proceed)
    }
}
