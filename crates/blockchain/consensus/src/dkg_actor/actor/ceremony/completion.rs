use super::*;

impl CeremonyState {
    pub(in crate::dkg_actor::actor) fn timeout_error(&self) -> eyre::Report {
        let log_target = if self.chain_finalized_mode {
            self.log_threshold
        } else {
            self.n
        };
        eyre::eyre!(
            "DKG ceremony timed out after {:?} (acks: {}/{}, logs: {}/{})",
            DKG_TIMEOUT,
            self.roles.acked_players.len(),
            self.player_threshold,
            self.finalized_logs.len(),
            log_target,
        )
    }
    pub(in crate::dkg_actor::actor) async fn seal_dealer(
        &mut self,
        clock: &impl Clock,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
        progress_tx: &Option<mpsc::UnboundedSender<DkgProgress>>,
    ) -> Result<MessageOutcome> {
        // A threshold is enough for liveness, but not enough to avoid publicly
        // revealing a healthy player's evaluation. Give the remaining players a
        // bounded ACK grace. Finalize immediately if everybody already ACKed.
        if self.roles.dealer.is_some()
            && self.roles.acked_players.len() >= self.player_threshold as usize
            && self.ack_collection_deadline.is_none()
        {
            self.ack_collection_deadline = Some(clock.current() + ACK_COLLECTION_GRACE);
            debug!(
                acks = self.roles.acked_players.len(),
                validators = self.n,
                grace_ms = ACK_COLLECTION_GRACE.as_millis(),
                "dealer quorum reached; collecting remaining ACKs before finalization"
            );
        }
        let ack_grace_elapsed = self
            .ack_collection_deadline
            .is_some_and(|at| clock.current().duration_since(at).is_ok());
        let current_process_delivery_complete =
            self.roles.unsent_shares.is_empty() && self.roles.restart_replay_shares.is_empty();
        if self.roles.dealer.is_some()
            && self.roles.acked_players.len() >= self.player_threshold as usize
            && (current_process_delivery_complete || ack_grace_elapsed)
        {
            let Some(d) = self.roles.dealer.take() else {
                return Ok(MessageOutcome::SkipIteration);
            };
            // Do not leave an elapsed timer permanently ready in the biased
            // select: after sealing, the actor must keep receiving dealer logs.
            self.ack_collection_deadline = None;
            let signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey> = d.finalize::<N3f1>();

            info!(
                acks = self.roles.acked_players.len(),
                player_threshold = self.player_threshold,
                "dealer finalized, broadcasting log"
            );

            // Verify our own log. In chain-finalized mode, the log is used only
            // after it appears in a finalized block and `DkgManager` feeds it
            // back. Thus local/P2P subsets cannot diverge from the canonical
            // output.
            if signed_log.clone().check(&self.info).is_none() {
                return Err(eyre::eyre!("our own finalized log failed verification"));
            }
            if !self.chain_finalized_mode {
                let _ = record_and_store_signed_dealer_log(
                    signed_log.clone(),
                    &self.info,
                    &mut self.finalized_logs,
                    &mut self.signed_finalized_logs,
                    "local",
                );
            }

            if let Some(progress_tx) = progress_tx {
                let _ = progress_tx.send(DkgProgress::LocalDealerLog(Bytes::from(
                    signed_log.encode(),
                )));
            }

            // Broadcast our finalized log to all peers.
            self.roles.unsent_shares.clear();
            self.roles.restart_replay_shares.clear();
            if !send_finalized_log(sender, self.ceremony_id, signed_log) {
                debug!("finalized-log broadcast had no accepting recipients this attempt (rate-limited/backpressure); recovered by the DKG retry tick");
            }
        }

        Ok(MessageOutcome::Advance)
    }
    pub(in crate::dkg_actor::actor) async fn bootstrap_complete(
        &mut self,
        clock: &impl Clock,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ) -> bool {
        // Non-chain interactive bootstrap: there is no canonical chain carrier yet.
        // Thus a validator completes only when it has collected ALL genesis dealer
        // logs. Threshold P2P subsets are not canonical and may otherwise produce
        // different public polynomials on different validators. Chain-finalized
        // reshare does NOT complete on the raw all-n count. It flows through the
        // observe gate below (C1). Thus the actor breaks at the same log-set prefix
        // at which `DkgManager` freezes `canonical_output`.
        if !self.chain_finalized_mode && self.finalized_logs.len() as u32 >= self.n {
            let now = clock.current();
            match self.bootstrap_all_logs_collected_at {
                // `SystemTime::duration_since` errors only if `collected_at` is in the
                // future relative to `now`. The runtime clock is monotonic across these
                // reads. Thus `unwrap_or_default()` (a zero elapsed) is safe and panic-free.
                // In the impossible skew case, it defers the grace by one loop iteration.
                Some(collected_at)
                    if now.duration_since(collected_at).unwrap_or_default()
                        >= BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE =>
                {
                    info!(
                        logs = self.finalized_logs.len(),
                        n = self.n,
                        "all bootstrap logs collected and gossip grace elapsed, completing ceremony"
                    );
                    return true;
                }
                Some(_) => {}
                None => {
                    info!(
                        logs = self.finalized_logs.len(),
                        n = self.n,
                        grace_ms = BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE.as_millis(),
                        "all bootstrap logs collected; keeping DKG channel alive for finalized-log gossip grace"
                    );
                    self.bootstrap_all_logs_collected_at = Some(now);
                    gossip_finalized_logs(sender, self.ceremony_id, &self.signed_finalized_logs)
                        .await;
                }
            }
        }

        false
    }
    pub(in crate::dkg_actor::actor) fn chain_complete(&mut self) -> bool {
        // Once the finalized chain carries a reconstructable threshold, an
        // interrupted local dealer must not hold recovery hostage. It must not
        // wait for ACKs from peers that have already completed this ceremony.
        // The canonical logs are the decision. Durable Player replay lets a target
        // participant recover its private share from them. Bootstrap still
        // requires the local dealer to finish because it has no chain-finalized
        // carrier.
        if (!self.chain_finalized_mode && self.roles.dealer.is_some())
            || (self.finalized_logs.len() as u32) < self.log_threshold
        {
            return false;
        }
        if !self.chain_finalized_mode {
            if !self.bootstrap_threshold_logged {
                info!(
                    logs = self.finalized_logs.len(),
                    log_threshold = self.log_threshold,
                    n = self.n,
                    "bootstrap DKG threshold logs collected; waiting for all genesis participants to keep output deterministic"
                );
                self.bootstrap_threshold_logged = true;
            }
            return false;
        }
        // C1: a signed log can still contain garbage. Probe public reconstruction
        // over the full canonical set once per new log count. Complete at the same
        // first-observe-success prefix that `DkgManager` freezes.
        if self.finalized_logs.len() <= self.last_reconstruct_probe_len {
            return false;
        }
        self.last_reconstruct_probe_len = self.finalized_logs.len();
        if chain_finalized_reconstructable(&self.info, &self.finalized_logs) {
            info!(
                logs = self.finalized_logs.len(),
                log_threshold = self.log_threshold,
                "chain-finalized content-valid threshold reached (observe ok); completing ceremony"
            );
            return true;
        }
        debug!(
            logs = self.finalized_logs.len(),
            log_threshold = self.log_threshold,
            "raw threshold reached but < 2f+1 content-valid dealer logs (a signed-but-garbage log is present); awaiting more chain-finalized logs"
        );
        false
    }
    pub(in crate::dkg_actor::actor) fn finalize(self) -> Result<DkgComplete> {
        // Log invalid dealers (if any were detected during the ceremony).
        if !self.invalid_dealers.is_empty() {
            warn!(
                count = self.invalid_dealers.len(),
                "DKG completed with {} dealers who sent invalid shares",
                self.invalid_dealers.len()
            );
        }

        // Player finalize: recover threshold share from collected logs.
        let mut finalize_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(self.info.clone());
        for (dealer_pk, log) in &self.finalized_logs {
            finalize_logs.record(dealer_pk.clone(), log.clone());
        }
        // A target participant completes only with its private threshold share.
        // Public `observe` remains the canonical-output gate above, but it is not a
        // substitute for local Player state and cannot promote a shareless validator.
        let (output, share) = self
            .roles
            .player
            .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                finalize_logs,
                &Sequential,
            )
            .map_err(|error| eyre::eyre!("player finalize failed: {error:?}"))?;

        info!("DKG ceremony complete - threshold material obtained");

        // Surface validators whose individual share evaluation was publicly
        // REVEALED during the ceremony. These validators were offline/non-acking, so
        // `feldman_desmedt` reveals their share so that recovery can complete. The
        // `DealerLog` artifacts permanently commit the reveals on-chain. A revealed
        // share makes the VRF threshold partial of that validator publicly
        // forgeable. The effect is bounded: VRF drives leader election/fairness, not
        // BFT safety, and the BLS individual aggregate stays authoritative. But
        // operators must rotate the consensus key of the affected validator.
        // Previously, nothing consumed `Output::revealed()`.
        let revealed = output.revealed();
        if !revealed.is_empty() {
            crate::metrics::record_dkg_revealed_shares(revealed.len());
            for pk in revealed.iter() {
                warn!(
                    target: "outbe::dkg",
                    revealed_validator = %pk,
                    "DKG: a validator's individual share was REVEALED (offline during the ceremony); \
                     its VRF threshold partial is now publicly forgeable - rotate this validator's \
                     consensus key"
                );
            }
        }

        Ok(DkgComplete {
            output,
            share,
            participants: self.participants,
        })
    }
}
