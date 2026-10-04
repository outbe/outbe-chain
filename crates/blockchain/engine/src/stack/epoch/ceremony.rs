//! Freeze a deterministic target and supervise durable DKG ceremonies.
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
    pub(super) async fn schedule_rotation(
        &mut self,
        ctx: &E,
        current_height: u64,
    ) -> Result<EventAction> {
        if let Some(target) = self.rotation.frozen_dkg_target.as_ref() {
            let activation_deadline = target
                .planned_activation_height
                .saturating_add(self.dkg_rotation_params.activation_grace_blocks);
            if frozen_dkg_target_expired(
                current_height,
                target.planned_activation_height,
                self.dkg_rotation_params.activation_grace_blocks,
            ) {
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

        if self.rotation.retry_frozen_dkg {
            self.rotation.retry_frozen_dkg = false;
            if let Some(target) = self.rotation.frozen_dkg_target.as_ref().cloned() {
                info!(
                    dkg_cycle = target.dkg_cycle,
                    planned_activation_height = target.planned_activation_height,
                    "retrying DKG for frozen target"
                );
                self.rotation.reshare_in_progress = true;
                outbe_consensus::metrics::record_dkg_status(1);

                match self.rotation.dkg_mux.register(target.dkg_cycle).await {
                    Ok((dkg_tx, dkg_rx)) => {
                        let round = target.dkg_cycle;
                        let tx = self.rotation.dkg_result_tx.clone();
                        let progress_tx = self.rotation.dkg_progress_tx.clone();
                        let key = self.signing_key.clone();
                        let parts = target.participants.clone();
                        // Share-less joiner: refresh prev_output from the chain so
                        // the ceremony info_hash matches the committee's (see
                        // refresh_verifier_join_prev_output).
                        if self.state.signing_share.is_none() {
                            refresh_verifier_join_prev_output(
                                &self.node.provider,
                                target.freeze_height,
                                self.dkg_rotation_params,
                                &mut self.state.last_dkg_output,
                            );
                        }
                        let prev_output = self.state.last_dkg_output.clone();
                        let prev_share = self.state.signing_share.clone();
                        let role = classify_local_reshare_role(
                            &key.public_key(),
                            prev_output.as_ref(),
                            &parts,
                        );
                        let (finalized_log_tx, finalized_log_rx) =
                            tokio::sync::mpsc::unbounded_channel();
                        if let Err(error) = restart_dkg_manager_from_finalized_history(
                            &self.node.provider,
                            &self.dkg_manager,
                            DkgCeremonyReplaySpec {
                                freeze_height: target.freeze_height,
                                epoch: self.state.current_epoch,
                                round,
                                previous_output: prev_output.clone(),
                                participants: target.participants.clone(),
                                finalized_dealer_log_tx: Some(finalized_log_tx.clone()),
                            },
                            || {
                                (*self.consensus_tip_rx.borrow()).expect(
                                                "the height arm continues before DKG retry when no consensus tip is available",
                                            )
                            },
                        ) {
                            warn!(%error, epoch = %self.state.current_epoch, round, "failed to recover DKG manager state for frozen-target retry");
                            self.rotation.reshare_in_progress = false;
                            self.rotation.retry_frozen_dkg = true;
                            outbe_consensus::metrics::record_dkg_status(0);
                            return Ok(EventAction::Continue);
                        }
                        let retry_store = dkg_retry_store(&self.args, &self.key_backend)?;
                        ctx.child("dkg_retry").spawn(move |dkg_ctx| async move {
                                        let result = match role {
                                            LocalDkgRole::DealerAndPlayer => {
                                                dkg_actor::run_initial_dkg_durable(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    prev_output,
                                                    prev_share,
                                                    round,
                                                    Some(progress_tx),
                                                    Some(finalized_log_rx),
                                                    retry_store.clone(),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::Complete)
                                            }
                                            LocalDkgRole::PlayerOnly => {
                                                dkg_actor::run_initial_dkg_durable(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    prev_output,
                                                    None,
                                                    round,
                                                    Some(progress_tx),
                                                    Some(finalized_log_rx),
                                                    retry_store.clone(),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::Complete)
                                            }
                                            LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                                                (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only_durable(
                                                    &dkg_ctx,
                                                    key,
                                                    parts,
                                                    output,
                                                    share,
                                                    round,
                                                    progress_tx,
                                                    retry_store.clone(),
                                                    dkg_tx,
                                                    dkg_rx,
                                                )
                                                .await
                                                .map(DkgTaskOutcome::DealerOnly),
                                                (None, _) => Err(eyre::eyre!(
                                                    "dealer-only DKG retry requires previous output"
                                                )),
                                                (Some(_), None) => Err(eyre::eyre!(
                                                    "dealer-only DKG requires a previous share"
                                                )),
                                            },
                                            LocalDkgRole::NotParticipant => Err(eyre::eyre!(
                                                "local key is neither previous dealer nor target player for DKG retry"
                                            )),
                                        };
                                        let _ = tx.send(result);
                                    });
                    }
                    Err(e) => {
                        warn!(?e, "failed to register DKG subchannel for retry");
                        self.rotation.reshare_in_progress = false;
                        self.rotation.retry_frozen_dkg = true;
                    }
                }
                return Ok(EventAction::Continue);
            }
        }

        let freeze_height = self
            .dkg_rotation_params
            .freeze_height(self.state.last_dkg_activation_height);
        if self.rotation.dealer_only_dkg_activation.is_none()
            && should_start_dkg_rotation(
                self.rotation.frozen_dkg_target.is_some(),
                self.rotation.pending_dkg_activation.is_some(),
                current_height,
                freeze_height,
            )
        {
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
            let (target_validator_set, target_participants, tee_expired_target_exclusions) =
                match refresh_validator_set_at_height(&self.node, freeze_height) {
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
                            // A share-less verifier-follower does not run a ceremony,
                            // but it observes the same finalized dealer logs, reconstructs
                            // the exact incoming output, publishes/validates the same
                            // preannounce, and crosses the same outgoing-finalized
                            // handoff as participants. Reusing the old polynomial at the
                            // planned height would bypass authentication and cannot follow
                            // membership-changing rotations safely.
                            if self.state.signing_share.is_none() {
                                let peer_map = build_peer_map(&new_set, &self.bootnode_map);
                                self.peer_manager_mailbox
                                    .prepare_dkg(peer_map)
                                    .await
                                    .wrap_err(
                                        "failed to publish verifier-follower DKG admission",
                                    )?;
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
                                let is_validator_set_change =
                                    new_participants != self.state.participants;
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
                                self.rotation.dealer_only_dkg_activation =
                                    Some(DealerOnlyDkgActivation {
                                        target,
                                        boundary_artifact: None,
                                        recovered_output: None,
                                    });
                                outbe_consensus::metrics::record_dkg_status(2);
                                let _ = self.execution_finalized_height_tx.send(current_height);
                                return Ok(EventAction::Continue);
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
                        (new_set, new_participants, tee_expired_target_exclusions)
                    }
                    Ok(FrozenValidatorSetRefresh::PendingBlockHash) => {
                        match pending_freeze_block_hash_decision(
                            current_height,
                            planned_activation_height,
                        ) {
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
                        return Ok(EventAction::Continue);
                    }
                    Err(e) => {
                        self.vrf_safety.mark_expired(current_height);
                        publish_randomness_status(&self.bridge, &self.vrf_safety);
                        return Err(eyre::eyre!(
                            "failed to refresh frozen validator set at height {freeze_height}: {e}"
                        ));
                    }
                };

            let is_validator_set_change = target_participants != self.state.participants;
            let target_dkg_cycle = self.state.dkg_cycle;
            self.rotation.frozen_dkg_target = Some(FrozenDkgTarget {
                dkg_cycle: target_dkg_cycle,
                freeze_height,
                planned_activation_height,
                validator_set: target_validator_set.clone(),
                participants: target_participants.clone(),
                tee_expired_target_exclusions,
                is_validator_set_change,
            });
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
                    let round = target_dkg_cycle;
                    let tx = self.rotation.dkg_result_tx.clone();
                    let progress_tx = self.rotation.dkg_progress_tx.clone();
                    let key = self.signing_key.clone();
                    let parts = target_participants.clone();
                    // Share-less joiner (verifier-join becoming a player): refresh
                    // prev_output from the chain's canonical output so the ceremony
                    // info_hash matches the committee's. Without this a long-lived
                    // TEE full-node joining at its FIRST reshare presents its stale
                    // CLI `--consensus.dkg-output` -> info_hash mismatch -> dealer
                    // bundles dropped -> timeout -> ACTIVE-but-voteless.
                    if self.state.signing_share.is_none() {
                        refresh_verifier_join_prev_output(
                            &self.node.provider,
                            freeze_height,
                            self.dkg_rotation_params,
                            &mut self.state.last_dkg_output,
                        );
                    }
                    // Capture previous DKG state for reshare.
                    let prev_output = self.state.last_dkg_output.clone();
                    let prev_share = self.state.signing_share.clone();
                    let role = classify_local_reshare_role(
                        &key.public_key(),
                        prev_output.as_ref(),
                        &parts,
                    );
                    let (finalized_log_tx, finalized_log_rx) =
                        tokio::sync::mpsc::unbounded_channel();
                    if let Err(error) = restart_dkg_manager_from_finalized_history(
                        &self.node.provider,
                        &self.dkg_manager,
                        DkgCeremonyReplaySpec {
                            freeze_height,
                            epoch: self.state.current_epoch,
                            round,
                            previous_output: prev_output.clone(),
                            participants: target_participants.clone(),
                            finalized_dealer_log_tx: Some(finalized_log_tx.clone()),
                        },
                        || {
                            (*self.consensus_tip_rx.borrow()).expect(
                                            "the height arm continues before live DKG recovery when no consensus tip is available",
                                        )
                        },
                    ) {
                        warn!(
                            %error,
                            epoch = %self.state.current_epoch,
                            round,
                            from_height = freeze_height,
                            scheduling_height = current_height,
                            "failed to recover live DKG manager state from finalized history"
                        );
                        self.rotation.reshare_in_progress = false;
                        self.rotation.retry_frozen_dkg = true;
                        outbe_consensus::metrics::record_dkg_status(0);
                        return Ok(EventAction::Continue);
                    }
                    let retry_store = dkg_retry_store(&self.args, &self.key_backend)?;
                    ctx.child("dkg_live").spawn(move |dkg_ctx| async move {
                                    let result = match role {
                                        LocalDkgRole::DealerAndPlayer => {
                                            dkg_actor::run_initial_dkg_durable(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                prev_output,
                                                prev_share,
                                                round,
                                                Some(progress_tx),
                                                Some(finalized_log_rx),
                                                retry_store.clone(),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::Complete)
                                        }
                                        LocalDkgRole::PlayerOnly => {
                                            dkg_actor::run_initial_dkg_durable(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                prev_output,
                                                None,
                                                round,
                                                Some(progress_tx),
                                                Some(finalized_log_rx),
                                                retry_store.clone(),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::Complete)
                                        }
                                        LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                                            (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only_durable(
                                                &dkg_ctx,
                                                key,
                                                parts,
                                                output,
                                                share,
                                                round,
                                                progress_tx,
                                                retry_store.clone(),
                                                dkg_tx,
                                                dkg_rx,
                                            )
                                            .await
                                            .map(DkgTaskOutcome::DealerOnly),
                                            (None, _) => Err(eyre::eyre!(
                                                "dealer-only live DKG requires previous output"
                                            )),
                                            (Some(_), None) => Err(eyre::eyre!(
                                                "dealer-only live DKG requires a previous share"
                                            )),
                                        },
                                        LocalDkgRole::NotParticipant => Err(eyre::eyre!(
                                            "local key is neither previous dealer nor target player for live DKG"
                                        )),
                                    };
                                    let _ = tx.send(result);
                                });
                }
                Err(e) => {
                    warn!(?e, "failed to register DKG subchannel");
                    self.rotation.reshare_in_progress = false;
                    self.rotation.frozen_dkg_target = None;
                }
            }
        } else {
            debug!(
                current_height,
                freeze_height, "DKG rotation freeze height not reached"
            );
        }

        Ok(EventAction::Proceed)
    }
}
