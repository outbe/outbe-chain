use super::*;

impl CeremonyState {
    pub(in crate::dkg_actor::actor) fn read_message(
        &self,
        from: &bls12381::PublicKey,
        buf: &mut impl bytes::Buf,
    ) -> Option<DkgMessage> {
        read_ceremony_message(from, buf, &self.wire_cfg, &PARTICIPANT_DIAGNOSTICS)
    }
    pub(in crate::dkg_actor::actor) async fn handle_message(
        &mut self,
        from: bls12381::PublicKey,
        msg: DkgMessage,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
        progress_tx: &Option<mpsc::UnboundedSender<DkgProgress>>,
    ) -> Result<MessageOutcome> {
        match msg {
            DkgMessage::DealerBundle {
                pub_msg, priv_msg, ..
            } => self.handle_bundle(from, pub_msg, priv_msg, sender).await?,
            DkgMessage::Ack { ack, .. } => self.handle_ack(from, ack)?,
            DkgMessage::FinalizedLog { signed_log, .. } => {
                return Ok(self.handle_finalized_log(from, signed_log, progress_tx))
            }
        }
        Ok(MessageOutcome::Advance)
    }
    async fn handle_bundle(
        &mut self,
        from: bls12381::PublicKey,
        pub_msg: DealerPubMsg<MinSig>,
        priv_msg: DealerPrivMsg,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ) -> Result<()> {
        // We are a Player receiving a dealing from another Dealer.
        match handle_player_bundle(
            &mut self.roles.player,
            &mut self.roles.player_retry_snapshot,
            self.retry_store.as_ref(),
            crate::dkg_actor::recovery::PlayerDealerBundle {
                dealer: from.clone(),
                pub_msg,
                priv_msg,
            },
        )? {
            PlayerBundleAction::SendAck(ack) => {
                send_ack(sender, self.ceremony_id, &from, ack, "sent ack to dealer").await;
            }
            PlayerBundleAction::DuplicateAck(ack) => {
                send_ack(
                    sender,
                    self.ceremony_id,
                    &from,
                    ack,
                    "resent cached ack for duplicate dealer bundle",
                )
                .await;
            }
            PlayerBundleAction::Equivocation { previous, received } => {
                warn!(
                    ?from,
                    previous_bundle_hash = %previous,
                    received_bundle_hash = %received,
                    "dealer sent conflicting DKG bundle"
                );
                self.invalid_dealers.insert(from.clone());
            }
            PlayerBundleAction::Invalid => {
                warn!(?from, "dealer sent invalid share - potential misbehavior");
                self.invalid_dealers.insert(from.clone());
            }
        }
        Ok(())
    }
    fn handle_ack(
        &mut self,
        from: bls12381::PublicKey,
        ack: PlayerAck<bls12381::PublicKey>,
    ) -> Result<()> {
        // We are a Dealer receiving an ack from a Player.
        // A byte-identical ACK for a recovered dealing confirms that
        // the restarted remote Player ingested our replay. It is
        // already present in Dealer state, so do not feed it twice.
        if let Some(ref mut d) = self.roles.dealer {
            if acknowledge_restart_replay(
                &mut self.roles.restart_replay_shares,
                self.roles
                    .dealer_retry_snapshot
                    .as_ref()
                    .map(|snapshot| &snapshot.accepted_acks),
                &from,
                &ack,
            ) {
                debug!(
                    ?from,
                    remaining = self.roles.restart_replay_shares.len(),
                    "restart replay confirmed by player"
                );
            } else {
                match d.receive_player_ack(from.clone(), ack.clone()) {
                    Ok(()) => {
                        let is_new_ack = self.roles.acked_players.insert(from.clone());
                        if is_new_ack {
                            persist_dealer_ack(
                                self.roles.dealer_retry_snapshot.as_mut(),
                                self.retry_store.as_ref(),
                                from.clone(),
                                ack,
                            )?;
                        }
                        self.roles.unsent_shares.remove(&from);
                        let acks_received = self.roles.acked_players.len();
                        debug!(
                            ?from,
                            acks_received,
                            player_threshold = self.player_threshold,
                            is_new_ack,
                            remaining = self.roles.unsent_shares.len(),
                            "received ack"
                        );
                    }
                    Err(e) => {
                        debug!(?e, ?from, "ack rejected");
                    }
                }
            }
        }
        Ok(())
    }
    fn handle_finalized_log(
        &mut self,
        from: bls12381::PublicKey,
        signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
        progress_tx: &Option<mpsc::UnboundedSender<DkgProgress>>,
    ) -> MessageOutcome {
        if self.chain_finalized_mode {
            if signed_log.clone().check(&self.info).is_none() {
                warn!(?from, "received invalid finalized log, ignoring");
            } else {
                if let Some(progress_tx) = progress_tx {
                    let _ = progress_tx
                        .send(DkgProgress::P2pDealerLog(Bytes::from(signed_log.encode())));
                }
                debug!(
                    ?from,
                    "received P2P finalized log; queued as proposal candidate"
                );
            }
            return MessageOutcome::SkipIteration;
        }
        if record_and_store_signed_dealer_log(
            signed_log,
            &self.info,
            &mut self.finalized_logs,
            &mut self.signed_finalized_logs,
            "p2p",
        )
        .is_none()
        {
            warn!(?from, "received invalid finalized log, ignoring");
        }
        MessageOutcome::Advance
    }
    pub(in crate::dkg_actor::actor) fn record_chain_log(&mut self, bytes: Bytes) {
        match decode_signed_dealer_log(&bytes, &self.wire_cfg.max_players) {
            Ok(signed_log) => {
                let _ = record_signed_dealer_log(
                    signed_log,
                    &self.info,
                    &mut self.finalized_logs,
                    "chain",
                );
            }
            Err(error) => {
                warn!(%error, "failed to decode chain-finalized DKG dealer log");
            }
        }
    }
    pub(in crate::dkg_actor::actor) async fn retry(
        &mut self,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ) {
        // Advance on the fixed interval schedule (matches the previous
        // interval-timer cadence, no drift from wakeup latency).
        self.next_retry_tick += RETRY_INTERVAL;
        if should_retry_share_distribution(
            self.roles.dealer.is_some(),
            &self.roles.restart_replay_shares,
        ) || should_retry_share_distribution(
            self.roles.dealer.is_some(),
            &self.roles.unsent_shares,
        ) {
            if let Some(my_pub_msg) = self.roles.my_pub_msg.as_ref() {
                debug!(
                    unacknowledged = self.roles.unsent_shares.len(),
                    restart_replays = self.roles.restart_replay_shares.len(),
                    "retrying share distribution"
                );
                send_shares(
                    sender,
                    self.ceremony_id,
                    my_pub_msg,
                    &self.roles.restart_replay_shares,
                )
                .await;
                send_shares(
                    sender,
                    self.ceremony_id,
                    my_pub_msg,
                    &self.roles.unsent_shares,
                )
                .await;
            }
        }
        if should_retry_finalized_log_gossip(self.chain_finalized_mode, &self.signed_finalized_logs)
        {
            debug!(
                logs = self.signed_finalized_logs.len(),
                target = self.n,
                "retrying bootstrap finalized-log gossip"
            );
            gossip_finalized_logs(sender, self.ceremony_id, &self.signed_finalized_logs).await;
        }
    }
}
