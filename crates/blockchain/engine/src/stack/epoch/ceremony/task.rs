//! Freeze a deterministic target and supervise durable DKG ceremonies.
#[derive(Clone, Copy)]
pub(super) enum CeremonyAttempt {
    Retry,
    Live { scheduling_height: u64 },
}
impl CeremonyAttempt {
    fn task_label(self) -> &'static str {
        match self {
            Self::Retry => "dkg_retry",
            Self::Live { .. } => "dkg_live",
        }
    }
    fn replay_precondition(self) -> &'static str {
        match self { Self::Retry => "the height arm continues before DKG retry when no consensus tip is available", Self::Live { .. } => "the height arm continues before live DKG recovery when no consensus tip is available" }
    }
    fn missing_output(self) -> &'static str {
        match self {
            Self::Retry => "dealer-only DKG retry requires previous output",
            Self::Live { .. } => "dealer-only live DKG requires previous output",
        }
    }
    fn missing_share(self) -> &'static str {
        match self {
            Self::Retry => "dealer-only DKG requires a previous share",
            Self::Live { .. } => "dealer-only live DKG requires a previous share",
        }
    }
    fn not_participant(self) -> &'static str {
        match self {
            Self::Retry => "local key is neither previous dealer nor target player for DKG retry",
            Self::Live { .. } => {
                "local key is neither previous dealer nor target player for live DKG"
            }
        }
    }
}
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
    pub(super) fn launch_rotation_ceremony(
        &mut self,
        ctx: &E,
        target: &FrozenDkgTarget,
        channels: super::super::transport::Subchannel<E>,
        attempt: CeremonyAttempt,
    ) -> Result<EventAction> {
        let (dkg_tx, dkg_rx) = channels;

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
        let role = classify_local_reshare_role(&key.public_key(), prev_output.as_ref(), &parts);
        let (finalized_log_tx, finalized_log_rx) = tokio::sync::mpsc::unbounded_channel();
        let replay_precondition = attempt.replay_precondition();
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
            || (*self.consensus_tip_rx.borrow()).expect(replay_precondition),
        ) {
            match attempt {
                CeremonyAttempt::Retry => {
                    warn!(%error, epoch = %self.state.current_epoch, round, "failed to recover DKG manager state for frozen-target retry")
                }
                CeremonyAttempt::Live { scheduling_height } => {
                    warn!(%error, epoch = %self.state.current_epoch, round, from_height = target.freeze_height, scheduling_height, "failed to recover live DKG manager state from finalized history")
                }
            };
            self.rotation.reshare_in_progress = false;
            self.rotation.retry_frozen_dkg = true;
            outbe_consensus::metrics::record_dkg_status(0);
            return Ok(EventAction::Continue);
        }
        let retry_store = dkg_retry_store(&self.args, &self.key_backend)?;
        ctx.child(attempt.task_label())
            .spawn(move |dkg_ctx| async move {
                let result = match role {
                    LocalDkgRole::DealerAndPlayer | LocalDkgRole::PlayerOnly => {
                        let prev_share = if role == LocalDkgRole::PlayerOnly {
                            None
                        } else {
                            prev_share
                        };

                        dkg_actor::run_initial_dkg_durable(
                            &dkg_ctx,
                            dkg_actor::DkgParticipantParameters {
                                signing_key: key,
                                participants: parts,
                                previous_output: prev_output,
                                previous_share: prev_share,
                                round,
                            },
                            dkg_actor::DkgProgressChannels {
                                progress_tx: Some(progress_tx),
                                finalized_log_rx: Some(finalized_log_rx),
                            },
                            retry_store.clone(),
                            dkg_actor::DkgTransport {
                                sender: dkg_tx,
                                receiver: dkg_rx,
                            },
                        )
                        .await
                        .map(DkgTaskOutcome::Complete)
                    }

                    LocalDkgRole::DealerOnly => match (prev_output, prev_share) {
                        (Some(output), Some(share)) => dkg_actor::run_reshare_dealer_only_durable(
                            &dkg_ctx,
                            dkg_actor::DkgDealerParameters {
                                signing_key: key,
                                participants: parts,
                                previous_output: output,
                                previous_share: share,
                                round,
                            },
                            progress_tx,
                            retry_store.clone(),
                            dkg_actor::DkgTransport {
                                sender: dkg_tx,
                                receiver: dkg_rx,
                            },
                        )
                        .await
                        .map(DkgTaskOutcome::DealerOnly),
                        (None, _) => Err(eyre::eyre!(attempt.missing_output())),
                        (Some(_), None) => Err(eyre::eyre!(attempt.missing_share())),
                    },
                    LocalDkgRole::NotParticipant => Err(eyre::eyre!(attempt.not_participant())),
                };
                let _ = tx.send(result);
            });
        Ok(EventAction::Proceed)
    }
}
