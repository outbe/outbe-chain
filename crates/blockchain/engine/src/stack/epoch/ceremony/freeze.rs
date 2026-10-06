//! Freeze a deterministic target and supervise durable DKG ceremonies.
type AdmittedRotationTarget = (
    validators::ValidatorSet,
    commonware_utils::ordered::Set<bls12381::PublicKey>,
    Vec<EthAddress>,
);
use super::*;

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
    pub(super) async fn freeze_new_rotation(
        &mut self,
        ctx: &E,
        current_height: u64,
        freeze_height: u64,
    ) -> Result<EventAction> {
        let planned_activation_height = self
            .dkg_rotation_params
            .planned_activation_height(self.state.last_dkg_activation_height);
        info!(
            dkg_cycle = self.state.dkg_cycle,
            current_height,
            freeze_height,
            planned_activation_height,
            "freezing validator set and starting DKG rotation"
        );

        // Freeze the target set from the EVM state at freeze_height.
        // This keeps DKG membership deterministic across validators.
        let Some((target_validator_set, target_participants, tee_expired_target_exclusions)) = self
            .acquire_rotation_target(current_height, freeze_height, planned_activation_height)
            .await?
        else {
            return Ok(EventAction::Continue);
        };
        let is_validator_set_change = target_participants != self.state.participants;
        let target_dkg_cycle = self.state.dkg_cycle;
        let target = FrozenDkgTarget {
            dkg_cycle: target_dkg_cycle,
            freeze_height,
            planned_activation_height,
            validator_set: target_validator_set.clone(),
            participants: target_participants.clone(),
            tee_expired_target_exclusions,
            is_validator_set_change,
        };
        self.rotation.frozen_dkg_target = Some(target.clone());
        self.vrf_safety.note_preparing(
            target_dkg_cycle,
            freeze_height,
            planned_activation_height,
            self.dkg_rotation_params.activation_grace_blocks,
        );
        publish_randomness_status(&self.bridge, &self.vrf_safety);
        self.state.dkg_cycle = target_dkg_cycle.saturating_add(1);

        self.rotation.reshare_in_progress = true;
        outbe_consensus::metrics::record_dkg_status(1); // in progress

        // Register DKG sub-channel for this reshare round.
        match self.rotation.dkg_mux.register(target_dkg_cycle).await {
            Ok((dkg_tx, dkg_rx)) => {
                return self.launch_rotation_ceremony(
                    ctx,
                    &target,
                    (dkg_tx, dkg_rx),
                    CeremonyAttempt::Live {
                        scheduling_height: current_height,
                    },
                );
            }
            Err(e) => {
                warn!(?e, "failed to register DKG subchannel");
                self.rotation.reshare_in_progress = false;
                self.rotation.frozen_dkg_target = None;
            }
        }
        Ok(EventAction::Proceed)
    }
    async fn acquire_rotation_target(
        &mut self,
        current_height: u64,
        freeze_height: u64,
        planned_activation_height: u64,
    ) -> Result<Option<AdmittedRotationTarget>> {
        let target = match refresh_validator_set_at_height(&self.node, freeze_height) {
            Ok(FrozenValidatorSetRefresh::Ready {
                validator_set: new_set,
                participants: new_participants,
                tee_expired_target_exclusions,
            }) => {
                let old_count = self.state.participants.len();
                let local_role = classify_local_reshare_role(
                    &self.signing_key.public_key(),
                    self.state.last_dkg_output.as_ref(),
                    &new_participants,
                );
                if local_role == LocalDkgRole::NotParticipant {
                    // A share-less verifier-follower does not run a ceremony.
                    // Like participants, it:
                    // - observes the same finalized dealer logs,
                    // - reconstructs the exact incoming output,
                    // - publishes/validates the same preannounce,
                    // - crosses the same outgoing-finalized handoff.
                    // Reusing the old polynomial at the planned height would
                    // bypass authentication and cannot follow
                    // membership-changing rotations safely.
                    if self.state.signing_share.is_none() {
                        let peer_map = build_peer_map(&new_set, &self.bootnode_map);
                        self.peer_manager_mailbox
                            .prepare_dkg(peer_map)
                            .await
                            .wrap_err("failed to publish verifier-follower DKG admission")?;
                        restart_dkg_manager_from_finalized_history(
                                    &self.node.provider,
                                    &self.dkg_manager,
                                    DkgCeremonyReplaySpec {
                                        freeze_height,
                                        epoch: self.state.current_epoch,
                                        round: self.state.dkg_cycle,
                                        previous_output: self.state.last_dkg_output.clone(),
                                        participants: new_participants.clone(),
                                        finalized_dealer_log_tx: None,
                                    },
                                    || {
                                        (*self.consensus_tip_rx.borrow()).expect(
                                            "the height arm continues before verifier-follower DKG recovery when no consensus tip is available",
                                        )
                                    },
                                )
                                        .wrap_err(
                                            "failed to recover verifier-follower DKG reconstruction from finalized history",
                                        )?;
                        let is_validator_set_change = new_participants != self.state.participants;
                        let target = FrozenDkgTarget {
                            dkg_cycle: self.state.dkg_cycle,
                            freeze_height,
                            planned_activation_height,
                            validator_set: new_set,
                            participants: new_participants,
                            tee_expired_target_exclusions,
                            is_validator_set_change,
                        };
                        info!(
                                            freeze_height,
                                            planned_activation_height,
                                            dkg_cycle = self.state.dkg_cycle,
                                            "verifier-follower: reconstructing pending DKG boundary before authenticated handoff"
                                        );
                        self.rotation.frozen_dkg_target = Some(target.clone());
                        self.rotation.dealer_only_dkg_activation = Some(DealerOnlyDkgActivation {
                            target,
                            boundary_artifact: None,
                            recovered_output: None,
                        });
                        outbe_consensus::metrics::record_dkg_status(2);
                        let _ = self.execution_finalized_height_tx.send(current_height);
                        return Ok(None);
                    }
                    return Err(eyre::eyre!(
                                        "local validator is neither previous DKG dealer nor frozen target player at height {freeze_height}"
                                    ));
                }

                // Update P2P oracle so new validators can participate in DKG.
                let peer_map = build_peer_map(&new_set, &self.bootnode_map);
                let dkg_peer_set_id = self
                    .peer_manager_mailbox
                    .prepare_dkg(peer_map)
                    .await
                    .wrap_err("failed to publish DKG admission")?;

                info!(
                    old = old_count,
                    new = new_participants.len(),
                    ?local_role,
                    dkg_peer_set_id,
                    tee_expired_target_exclusions = tee_expired_target_exclusions.len(),
                    "refreshed validator set from EVM state for reshare"
                );
                Some((new_set, new_participants, tee_expired_target_exclusions))
            }
            Ok(FrozenValidatorSetRefresh::PendingBlockHash) => {
                match pending_freeze_block_hash_decision(current_height, planned_activation_height)
                {
                    PendingFreezeBlockHashDecision::Retry => {}
                    PendingFreezeBlockHashDecision::Expired => {
                        self.vrf_safety.mark_expired(current_height);
                        publish_randomness_status(&self.bridge, &self.vrf_safety);
                        return Err(eyre::eyre!(
                                            "frozen validator set block hash unavailable by planned activation: freeze height {freeze_height}, current height {current_height}, planned activation {planned_activation_height}"
                                        ));
                    }
                }
                warn!(
                                    current_height,
                                    freeze_height,
                                    planned_activation_height,
                                    "frozen validator set block hash is not available yet; retrying on next finalized height"
                                );
                return Ok(None);
            }
            Err(e) => {
                self.vrf_safety.mark_expired(current_height);
                publish_randomness_status(&self.bridge, &self.vrf_safety);
                return Err(eyre::eyre!(
                    "failed to refresh frozen validator set at height {freeze_height}: {e}"
                ));
            }
        };

        Ok(target)
    }
}
