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
        let Some(snapshot) = recovered_pending_boundary else {
            return Ok(());
        };
        let pending_epoch = Epoch::new(snapshot.artifact.epoch);
        let keys_dir = self
            .args
            .keys_dir
            .as_ref()
            .ok_or(crate::errors::StackEpochError::RecoveredPendingBoundaryWithoutKeysDir)?
            .clone();
        self.dkg_manager
            .note_recovered_pending_boundary(snapshot.artifact.clone());
        let restored = restore_pending_dkg_activation(
            snapshot,
            &keys_dir,
            &self.key_backend,
            &self.signing_key.public_key(),
            &self.node,
        )?;
        let (pending_artifact, restored_target_dkg_cycle) =
            restored_boundary_artifact_and_cycle(&restored)?;
        let exact_carrier_height = find_exact_finalized_preannounce_carrier(
            &self.node.provider,
            pending_artifact,
            recovery_anchor_height,
            self.dkg_rotation_params.activation_grace_blocks,
        )?;
        let Some(startup_plan) = self.plan_recovered_pending_handoff(
            pending_epoch,
            recovery_anchor_height,
            pending_artifact.planned_activation_height,
            exact_carrier_height,
            &keys_dir,
        )?
        else {
            return Ok(());
        };

        match startup_plan {
            StartupPendingDkgEpochPlan::Defer {
                active_epoch,
                preregister_after_current,
            } => self.defer_recovered_pending_handoff(
                restored,
                restored_target_dkg_cycle,
                active_epoch,
                preregister_after_current,
                recovery_anchor_height,
            ),
            StartupPendingDkgEpochPlan::Activate {
                previous_epoch,
                active_epoch,
                activation_anchor,
            } => {
                self.activate_recovered_pending_handoff(
                    restored,
                    previous_epoch,
                    active_epoch,
                    activation_anchor,
                    recovery_anchor_height,
                )
                .await
            }
        }
    }

    fn plan_recovered_pending_handoff(
        &self,
        pending_epoch: Epoch,
        recovery_anchor_height: u64,
        planned_activation_height: u64,
        exact_carrier_height: Option<u64>,
        keys_dir: &std::path::Path,
    ) -> Result<Option<StartupPendingDkgEpochPlan>> {
        match startup_pending_dkg_epoch_plan(
            self.state.current_epoch,
            pending_epoch,
            crate::stack::dkg::handoff::StartupPendingDkgHandoff {
                finalized_height: recovery_anchor_height,
                planned_activation_height,
                activation_grace_blocks: self.dkg_rotation_params.activation_grace_blocks,
                exact_carrier_height,
            },
        ) {
            Ok(plan) => Ok(Some(plan)),
            Err(error) if is_retirable_startup_handoff_error(&error) => {
                warn!(
                    %error,
                    active_epoch = %self.state.current_epoch,
                    %pending_epoch,
                    finalized_height = recovery_anchor_height,
                    "retiring stale or expired local pending DKG handoff during startup recovery"
                );
                let _ = retire_activated_dkg_retry_state(keys_dir, &self.key_backend);
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn defer_recovered_pending_handoff(
        &mut self,
        restored: RestoredPendingDkgActivation,
        restored_target_dkg_cycle: u64,
        active_epoch: Epoch,
        preregister_after_current: Epoch,
        recovery_anchor_height: u64,
    ) -> Result<()> {
        if active_epoch != self.state.current_epoch {
            return Err(
                crate::errors::StackEpochError::DeferredStartupPlanChangedActiveEpoch.into(),
            );
        }
        self.state.dkg_cycle =
            next_dkg_cycle_after_restored_target(self.state.dkg_cycle, restored_target_dkg_cycle);
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
        Ok(())
    }

    async fn activate_recovered_pending_handoff(
        &mut self,
        restored: RestoredPendingDkgActivation,
        previous_epoch: Epoch,
        active_epoch: Epoch,
        activation_anchor: u64,
        recovery_anchor_height: u64,
    ) -> Result<()> {
        let (target, canonical_output, activated_signing_share, boundary_artifact) =
            unpack_restored_pending_activation(restored)?;
        if boundary_artifact.epoch != active_epoch.get() {
            return Err(
                crate::errors::StackEpochError::RecoveredBoundaryEpochMismatchWithStartup {
                    boundary_epoch: boundary_artifact.epoch,
                    active_epoch,
                }
                .into(),
            );
        }
        let activated_vrf_material_version =
            outbe_validatorset::next_vrf_material_version(self.state.vrf_material_version)?;
        if boundary_artifact.vrf_material_version != activated_vrf_material_version {
            return Err(
                crate::errors::StackEpochError::RecoveredVrfVersionMismatch {
                    boundary_version: boundary_artifact.vrf_material_version,
                    active_version: self.state.vrf_material_version,
                }
                .into(),
            );
        }
        let activated_validator_set =
            validator_set_for_dkg_output_players(&canonical_output, &target.validator_set)?;
        let activated_participants = participants_from_validator_set(&activated_validator_set)?;

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
            EpochValidationCommittee {
                epoch: active_epoch,
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
        let recovered_peer_map = build_peer_map(&activated_validator_set, &self.bootnode_map);
        if self.peer_manager_mailbox.overwrite(recovered_peer_map) != commonware_actor::Feedback::Ok
        {
            return Err(
                crate::errors::StackEpochError::PeerManagerClosedDuringRecoveryAdmission.into(),
            );
        }
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
        self.observe_activated_boundary(active_epoch, activation_anchor);
        self.publish_repository_material();
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
                format!("pre-register activated startup subchannels for epoch {active_epoch}")
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
        Ok(())
    }
}

fn restored_boundary_artifact_and_cycle(
    restored: &RestoredPendingDkgActivation,
) -> Result<(&DkgBoundaryArtifact, u64)> {
    match restored {
        RestoredPendingDkgActivation::Participant(pending) => {
            Ok((&pending.boundary_artifact, pending.target.dkg_cycle))
        }
        RestoredPendingDkgActivation::DealerOnly(pending) => Ok((
            pending.boundary_artifact.as_ref().ok_or(
                crate::errors::StackEpochError::RecoveredDealerOnlyHandoffMissingBoundaryArtifact,
            )?,
            pending.target.dkg_cycle,
        )),
    }
}

fn is_retirable_startup_handoff_error(error: &eyre::Report) -> bool {
    matches!(
        error.downcast_ref::<crate::errors::StackDkgError>(),
        Some(
            crate::errors::StackDkgError::PendingEpochDoesNotFollowActive { .. }
                | crate::errors::StackDkgError::PendingEpochMissedActivationDeadline { .. }
        )
    )
}

#[allow(clippy::type_complexity)]
fn unpack_restored_pending_activation(
    restored: RestoredPendingDkgActivation,
) -> Result<(
    FrozenDkgTarget,
    Output<MinSig, bls12381::PublicKey>,
    Option<commonware_cryptography::bls12381::primitives::group::Share>,
    DkgBoundaryArtifact,
)> {
    match restored {
        RestoredPendingDkgActivation::Participant(pending) => Ok((
            pending.target,
            pending.complete.output,
            Some(pending.complete.share),
            pending.boundary_artifact,
        )),
        RestoredPendingDkgActivation::DealerOnly(pending) => {
            let boundary_artifact = pending.boundary_artifact.ok_or(
                crate::errors::StackEpochError::RecoveredDealerOnlyActivationMissingBoundaryArtifact,
            )?;
            let output = decode_boundary_output(&boundary_artifact)
                .wrap_err("failed to decode recovered dealer-only DKG activation output")?;
            Ok((pending.target, output, None, boundary_artifact))
        }
    }
}
