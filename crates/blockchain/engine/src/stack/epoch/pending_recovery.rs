//! Restore a pending handoff without advancing authority prematurely.
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
    pub(super) async fn restore_pending(
        &mut self,
        recovered_pending_boundary: Option<PendingDkgBoundarySnapshot>,
        recovery_anchor_height: u64,
    ) -> Result<()> {
        if let Some(snapshot) = recovered_pending_boundary {
            let pending_epoch = Epoch::new(snapshot.artifact.epoch);
            let keys_dir = self.args.keys_dir.as_ref().ok_or_else(|| {
                eyre::eyre!("recovered pending DKG boundary without configured keys directory")
            })?;
            self.dkg_manager
                .note_recovered_pending_boundary(snapshot.artifact.clone());
            let restored = restore_pending_dkg_activation(
                snapshot,
                keys_dir,
                &self.key_backend,
                &self.signing_key.public_key(),
                &self.node,
            )?;
            let pending_artifact = match &restored {
                RestoredPendingDkgActivation::Participant(pending) => &pending.boundary_artifact,
                RestoredPendingDkgActivation::DealerOnly(pending) => {
                    pending.boundary_artifact.as_ref().ok_or_else(|| {
                        eyre::eyre!("recovered dealer-only DKG handoff has no boundary artifact")
                    })?
                }
            };
            let restored_target_dkg_cycle = match &restored {
                RestoredPendingDkgActivation::Participant(pending) => pending.target.dkg_cycle,
                RestoredPendingDkgActivation::DealerOnly(pending) => pending.target.dkg_cycle,
            };
            let exact_carrier_height = find_exact_finalized_preannounce_carrier(
                &self.node.provider,
                pending_artifact,
                recovery_anchor_height,
                self.dkg_rotation_params.activation_grace_blocks,
            )?;
            let startup_plan = startup_pending_dkg_epoch_plan(
                self.state.current_epoch,
                pending_epoch,
                recovery_anchor_height,
                pending_artifact.planned_activation_height,
                self.dkg_rotation_params.activation_grace_blocks,
                exact_carrier_height,
            )?;

            match startup_plan {
                StartupPendingDkgEpochPlan::Defer {
                    active_epoch,
                    preregister_after_current,
                } => {
                    ensure!(
                        active_epoch == self.state.current_epoch,
                        "deferred startup DKG plan changed active epoch"
                    );
                    self.state.dkg_cycle = next_dkg_cycle_after_restored_target(
                        self.state.dkg_cycle,
                        restored_target_dkg_cycle,
                    );
                    match restored {
                        RestoredPendingDkgActivation::Participant(pending) => {
                            self.rotation.frozen_dkg_target = Some(pending.target.clone());
                            self.rotation.pending_dkg_activation = Some(pending);
                        }
                        RestoredPendingDkgActivation::DealerOnly(pending) => {
                            self.rotation.frozen_dkg_target = Some(pending.target.clone());
                            self.rotation.dealer_only_dkg_activation = Some(pending);
                        }
                    }
                    self.rotation.deferred_startup_pending_epoch = Some(preregister_after_current);
                    info!(
                        active_epoch = %self.state.current_epoch,
                        pending_epoch = %preregister_after_current,
                        finalized_height = recovery_anchor_height,
                        "restored future DKG handoff; current-epoch channels will be acquired first"
                    );
                }
                StartupPendingDkgEpochPlan::Activate {
                    previous_epoch,
                    active_epoch,
                    activation_anchor,
                } => {
                    let (target, canonical_output, activated_signing_share, boundary_artifact) =
                        match restored {
                            RestoredPendingDkgActivation::Participant(pending) => (
                                pending.target,
                                pending.complete.output,
                                Some(pending.complete.share),
                                pending.boundary_artifact,
                            ),
                            RestoredPendingDkgActivation::DealerOnly(pending) => {
                                let boundary_artifact =
                                    pending.boundary_artifact.ok_or_else(|| {
                                        eyre::eyre!(
                                    "recovered dealer-only DKG activation has no boundary artifact"
                                )
                                    })?;
                                let output = decode_boundary_output(&boundary_artifact).wrap_err(
                                    "failed to decode recovered dealer-only DKG activation output",
                                )?;
                                (pending.target, output, None, boundary_artifact)
                            }
                        };
                    ensure!(
                        boundary_artifact.epoch == active_epoch.get(),
                        "recovered DKG boundary epoch {} does not match activated startup epoch {}",
                        boundary_artifact.epoch,
                        active_epoch
                    );
                    let activated_vrf_material_version =
                        outbe_validatorset::next_vrf_material_version(
                            self.state.vrf_material_version,
                        )?;
                    ensure!(
                        boundary_artifact.vrf_material_version == activated_vrf_material_version,
                        "recovered DKG VRF material version {} does not follow active version {}",
                        boundary_artifact.vrf_material_version,
                        self.state.vrf_material_version
                    );
                    let activated_validator_set = validator_set_for_dkg_output_players(
                        &canonical_output,
                        &target.validator_set,
                    )?;
                    let activated_participants =
                        participants_from_validator_set(&activated_validator_set)?;

                    self.state.signing_share = activated_signing_share;
                    self.state.polynomial = canonical_output.public().clone();
                    self.state.last_dkg_output = Some(canonical_output.clone());
                    self.state.vrf_material_version = activated_vrf_material_version;
                    activate_vrf_material_and_publish_local_share(
                        &self.bridge,
                        &self.vrf_materials,
                        self.state.vrf_material_version,
                        self.state.polynomial.clone(),
                        self.state.signing_share.clone(),
                    );
                    register_epoch_validation_providers(
                        active_epoch,
                        &activated_participants,
                        &activated_validator_set,
                        None,
                        &self.vrf_materials,
                        &self.certificate_scheme_provider,
                        &self.committee_provider,
                    )?;
                    let recovered_peer_map =
                        build_peer_map(&activated_validator_set, &self.bootnode_map);
                    eyre::ensure!(
                        self.peer_manager_mailbox.overwrite(recovered_peer_map)
                            == commonware_actor::Feedback::Ok,
                        "peer_manager closed during recovery admission"
                    );
                    self.state.validator_set = activated_validator_set;
                    self.state.participants = activated_participants;
                    self.state.dkg_cycle = target.dkg_cycle.saturating_add(1);
                    self.state.last_dkg_activation_height = activation_anchor;
                    self.vrf_safety.note_activated(
                        self.state.vrf_material_version,
                        activation_anchor,
                        self.dkg_rotation_params
                            .planned_activation_height(activation_anchor),
                        self.dkg_rotation_params.activation_grace_blocks,
                    );
                    publish_randomness_status(&self.bridge, &self.vrf_safety);
                    self.application_epoch_fence
                        .arm_activation_boundary(previous_epoch, activation_anchor);
                    self.application_epoch_fence.advance_epoch(active_epoch);
                    self.state.current_epoch = active_epoch;
                    self.rotation.frozen_dkg_target = None;
                    self.rotation.pending_dkg_activation = None;
                    self.rotation.dealer_only_dkg_activation = None;
                    self.channels.next_epoch_subchannels = Some(
                        outbe_consensus::epoch_subchannels::register_epoch_subchannels(
                            active_epoch,
                            &mut self.channels.vote_mux,
                            &mut self.channels.cert_mux,
                            &mut self.channels.res_mux,
                        )
                        .await
                        .wrap_err_with(|| {
                            format!(
                            "pre-register activated startup subchannels for epoch {active_epoch}"
                        )
                        })?,
                    );
                    info!(
                        previous_epoch = %previous_epoch,
                        active_epoch = %active_epoch,
                        activation_anchor,
                        finalized_height = recovery_anchor_height,
                        vrf_material_version = self.state.vrf_material_version,
                        dkg_cycle = target.dkg_cycle,
                        dkg_output_hash = %dkg_manager::dkg_output_hash(&canonical_output),
                        "restored preannounce-authorized DKG activation before boundary commit"
                    );
                }
            }
        }

        Ok(())
    }
}
