use super::*;

impl CeremonyState {
    pub(in crate::dkg_actor::actor) async fn start(
        clock: &impl Clock,
        config: CeremonyConfig,
        chain_finalized_mode: bool,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ) -> Result<Self> {
        let CeremonyConfig {
            signing_key,
            participants,
            previous_output,
            previous_share,
            round,
            retry_store,
        } = config;
        let n = participants.len() as u32;
        let my_pk = commonware_cryptography::Signer::public_key(&signing_key);
        let player_threshold = participants.quorum::<N3f1>();

        let is_reshare = previous_output.is_some();
        let dealers = previous_output
            .as_ref()
            .map(|output| output.players().clone())
            .unwrap_or_else(|| participants.clone());
        let log_threshold = dealers.quorum::<N3f1>().max(
            previous_output
                .as_ref()
                .map(Output::quorum::<N3f1>)
                .unwrap_or(0),
        );
        let is_local_dealer = dealers.position(&my_pk).is_some();
        info!(
            validators = n,
            dealers = dealers.len(),
            player_threshold,
            log_threshold,
            is_reshare,
            is_local_dealer,
            round,
            "starting DKG ceremony"
        );

        let ceremony_id = DkgCeremonyId::new(
            &crate::config::outbe_app_namespace(),
            round,
            previous_output.as_ref(),
            &participants,
        );

        // Build DKG Info with optional previous output for reshare.
        // For initial: round=0, previous=None.
        // For reshare: round>0, previous=Some(Output), dealers must be from previous players.
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &crate::config::outbe_app_namespace(),
            round,
            previous_output,
            Mode::NonZeroCounter,
            commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
            dealers,
            participants.clone(),
        )
        .map_err(|e| eyre::eyre!("failed to create DKG info: {e:?}"))?;

        let dealer_retry_snapshot = if is_local_dealer {
            Some(load_or_create_dealer_snapshot(
                retry_store.as_ref(),
                ceremony_id,
                round,
                "restoring durable DKG dealer transcript",
            )?)
        } else {
            None
        };

        // Old share holders are dealers in a reshare. New validators are
        // players only until they receive a fresh threshold share.
        let (dealer, my_pub_msg, priv_msgs) = if is_local_dealer {
            let dealer_seed = dealer_retry_snapshot
                .as_ref()
                .ok_or_else(|| eyre::eyre!("local dealer retry snapshot was not initialized"))?
                .seed;
            let (dealer, my_pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_commonware::rngs::ChaCha20Rng::from_seed(dealer_seed),
                    info.clone(),
                    signing_key.clone(),
                    previous_share,
                )
                .map_err(|e| eyre::eyre!("failed to start dealer: {e:?}"))?;
            (Some(dealer), Some(my_pub_msg), priv_msgs)
        } else {
            if previous_share.is_some() {
                warn!(
                    "local validator has previous DKG share but is not a dealer in this ceremony"
                );
            }
            info!("local validator is DKG player-only for this reshare");
            (None, None, Vec::new())
        };

        let max_players = NonZeroU32::new(n)
            .ok_or_else(|| eyre::eyre!("DKG ceremony requires at least one participant"))?;
        // Reconstruct every previously acknowledged dealing before processing new
        // traffic. The snapshot is ceremony-scoped and replayed in dealer-key order.
        let (player, player_retry_snapshot) = restore_player(
            info.clone(),
            signing_key.clone(),
            ceremony_id,
            max_players,
            retry_store.as_ref(),
        )?;

        // Build a map of unsent private shares: public_key -> DealerPrivMsg.
        let unsent_shares: BTreeMap<bls12381::PublicKey, _> = priv_msgs.into_iter().collect();

        // Use a BTreeSet for unique ack tracking instead of a counter
        // (BTreeSet, not HashSet - deterministic iteration order on the consensus path).
        // Start empty - only count self-ack if self-dealing succeeded below.
        let acked_players: std::collections::BTreeSet<bls12381::PublicKey> =
            std::collections::BTreeSet::new();

        let mut roles = RecoveredRoles {
            dealer,
            my_pub_msg,
            dealer_retry_snapshot,
            player,
            player_retry_snapshot,
            unsent_shares,
            acked_players,
            restart_replay_shares: BTreeMap::new(),
        };
        roles.self_deal(&my_pk, retry_store.as_ref())?;
        roles.replay_remote_acks(&my_pk)?;
        if let Some(my_pub_msg) = roles.my_pub_msg.as_ref() {
            send_shares(
                sender,
                ceremony_id,
                my_pub_msg,
                &roles.restart_replay_shares,
            )
            .await;
            send_shares(sender, ceremony_id, my_pub_msg, &roles.unsent_shares).await;
        }
        // Arm the deadline only after recovery and the initial network sends.
        // The first retry stays one full interval out, on a fixed cadence.
        let now = clock.current();
        Ok(Self {
            info,
            ceremony_id,
            participants,
            n,
            player_threshold,
            log_threshold,
            wire_cfg: DkgWireConfig {
                max_players,
                expected_ceremony_id: ceremony_id,
            },
            retry_store,
            roles,
            finalized_logs: BTreeMap::new(),
            signed_finalized_logs: BTreeMap::new(),
            invalid_dealers: BTreeSet::new(),
            chain_finalized_mode,
            deadline: now + DKG_TIMEOUT,
            next_retry_tick: now + RETRY_INTERVAL,
            ack_collection_deadline: None,
            last_reconstruct_probe_len: 0,
            bootstrap_threshold_logged: false,
            bootstrap_all_logs_collected_at: None,
        })
    }
}

impl RecoveredRoles {
    fn self_deal(
        &mut self,
        my_pk: &bls12381::PublicKey,
        retry_store: Option<&DkgRetryStore>,
    ) -> Result<()> {
        // Handle self-dealing locally (no network round-trip).
        // Only count self-ack if self-dealing validation succeeds.
        if let (Some(my_pub_msg), Some(my_priv_msg)) =
            (self.my_pub_msg.as_ref(), self.unsent_shares.remove(my_pk))
        {
            if let PlayerBundleAction::SendAck(generated_ack)
            | PlayerBundleAction::DuplicateAck(generated_ack) = handle_player_bundle(
                &mut self.player,
                &mut self.player_retry_snapshot,
                retry_store,
                crate::dkg_actor::recovery::PlayerDealerBundle {
                    dealer: my_pk.clone(),
                    pub_msg: my_pub_msg.clone(),
                    priv_msg: my_priv_msg,
                },
            )? {
                if let Some(ref mut d) = self.dealer {
                    let recovered_ack = self
                        .dealer_retry_snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.accepted_acks.get(my_pk))
                        .cloned();
                    let is_recovered = recovered_ack.is_some();
                    let ack = recovered_ack.unwrap_or_else(|| generated_ack.clone());
                    d.receive_player_ack(my_pk.clone(), ack)
                        .map_err(|e| eyre::eyre!("failed to process self-ack: {e:?}"))?;
                    self.acked_players.insert(my_pk.clone());
                    if !is_recovered {
                        persist_dealer_ack(
                            self.dealer_retry_snapshot.as_mut(),
                            retry_store,
                            my_pk.clone(),
                            generated_ack,
                        )?;
                    }
                }
                debug!("self-dealing complete");
            } else {
                warn!("self-dealing validation failed");
            }
        }

        Ok(())
    }
    fn replay_remote_acks(&mut self, my_pk: &bls12381::PublicKey) -> Result<()> {
        // Rehydrate accepted remote ACKs before the first periodic retry. An ACK is
        // durable evidence for this local Dealer, but it does not prove that the
        // remote process-local Player state survived the same restart. Preserve each
        // acknowledged dealing in a separate byte-identical replay set until the
        // restarted Player confirms current-process ingestion with the same ACK.
        if let (Some(d), Some(snapshot)) =
            (self.dealer.as_mut(), self.dealer_retry_snapshot.as_ref())
        {
            self.restart_replay_shares =
                take_restart_replay_shares(&mut self.unsent_shares, &snapshot.accepted_acks);
            for (player_pk, ack) in &snapshot.accepted_acks {
                if player_pk == my_pk {
                    continue;
                }
                d.receive_player_ack(player_pk.clone(), ack.clone())
                    .map_err(|error| eyre::eyre!("failed to replay durable DKG ACK: {error:?}"))?;
                self.acked_players.insert(player_pk.clone());
            }
        }

        Ok(())
    }
}
