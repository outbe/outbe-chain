use super::*;

/// Enclave identity advertised during discovery or the signed exchange.
pub struct LocalIdentityAnnouncement {
    pub bls: Vec<u8>,
    pub enc: [u8; 32],
    pub signature: Vec<u8>,
}
/// Binding attached to the canonical participant announcements returned by
/// an exchange. Preliminary discovery uses the existing zero binding.
pub struct CeremonyBinding {
    pub ceremony_id: B256,
    pub round: u64,
    pub participant_set_hash: B256,
}
/// Local announcement, ceremony binding and expected participant count for
/// one identity-exchange phase.
pub struct IdentityExchange {
    pub local: LocalIdentityAnnouncement,
    pub binding: CeremonyBinding,
    pub participant_count: usize,
}
type IdentitySet = BTreeMap<Vec<u8>, ([u8; 32], Vec<u8>)>;
struct IdentityCollection {
    ids: IdentitySet,
    require_scoped: bool,
    expected_count: usize,
}
struct IdentityFrame<'a> {
    from: bls12381::PublicKey,
    bytes: &'a [u8],
}

impl IdentityExchange {
    fn announcements(self, ids: IdentitySet) -> Vec<outbe_tee::protocol::ParticipantAnnounce> {
        ids.into_iter()
            .map(
                |(bls, (enc, sig))| outbe_tee::protocol::ParticipantAnnounce {
                    bls_pub: bls,
                    enc_pub: enc,
                    ceremony_id: self.binding.ceremony_id,
                    round: self.binding.round,
                    participant_set_hash: self.binding.participant_set_hash,
                    enc_sig: sig,
                },
            )
            .collect()
    }
}

impl<S, R, C> CommonwareDkgGossip<S, R, C>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    /// Announce this enclave's `(tee_bls, dkg_enc)` identity and collect the
    /// identities of all `n` participants. During the exchange, this method also:
    ///
    /// - buffers any ceremony messages that arrive early,
    /// - records the `tee_bls -> consensus_pubkey` routing.
    ///
    /// Returns the identities sorted canonically by `tee_bls` (so every node
    /// derives the same ceremony id and participant order).
    pub async fn exchange_identities(
        &mut self,
        request: IdentityExchange,
    ) -> eyre::Result<Vec<outbe_tee::protocol::ParticipantAnnounce>> {
        self.exchange_identity_request(request).await
    }

    fn begin_identity_exchange(
        &mut self,
        request: &IdentityExchange,
    ) -> eyre::Result<IdentityCollection> {
        let local = &request.local;
        let require_scoped = !local.signature.is_empty();
        let mut ids = if require_scoped {
            std::mem::take(&mut self.early_signed_identities)
        } else {
            BTreeMap::new()
        };
        ids.insert(local.bls.clone(), (local.enc, local.signature.clone()));
        let mut env = vec![DKG_ENV_IDENTITY];
        env.extend_from_slice(&(local.bls.len() as u32).to_be_bytes());
        env.extend_from_slice(&local.bls);
        env.extend_from_slice(&local.enc);
        env.extend_from_slice(&(local.signature.len() as u32).to_be_bytes());
        env.extend_from_slice(&local.signature);
        let delivery_env = self
            .delivery
            .envelope(Recipients::All, env.clone())
            .map_err(|error| eyre::eyre!(error))?;
        let _ = self.sender.send(Recipients::All, delivery_env, true);
        Ok(IdentityCollection {
            ids,
            require_scoped,
            expected_count: request.participant_count,
        })
    }

    async fn exchange_identity_request(
        &mut self,
        request: IdentityExchange,
    ) -> eyre::Result<Vec<outbe_tee::protocol::ParticipantAnnounce>> {
        let mut collected = self.begin_identity_exchange(&request)?;
        let n = request.participant_count;
        // Re-broadcast our identity periodically until we collect the identity of
        // every peer. The muxed sub-channel drops messages addressed to a round that
        // a peer has not yet registered. Without retries, a node that announces before
        // its peers register would be lost, and the exchange would hang. Retrying lets
        // the exchange survive that registration race on every round.
        // The consensus runtime `Clock` measures the poll cadence, not tokio's
        // wall-clock. The deterministic test runtime can mock and advance this same
        // time source. This keeps the identity-exchange re-announce loop
        // reproducible and free of a direct async-runtime timer dependency. `select!`
        // is biased top-to-bottom, so it prefers a ready message over the tick.
        // On the tick arm, `select!` drops the in-flight `recv` future (cancel-safe
        // on this receiver, so no buffered message is lost).
        const POLL: std::time::Duration = std::time::Duration::from_millis(750);
        let mut idle_ticks = 0u32;
        while collected.ids.len() < n {
            commonware_macros::select! {
                recv = self.receiver.recv() => {
                    match recv {
                        Ok((from, raw)) => {
                            let Some((id, bytes)) = receive_delivery(&mut self.delivery, &from, raw.as_ref()) else { continue; };
                            let _ = self.sender.send(Recipients::One(from.clone()), ack_envelope(id), true);
                            self.handle_identity_frame(IdentityFrame { from, bytes }, &mut collected)?;
                        }
                        Err(_) => {
                            return Err(eyre::eyre!(
                                "TEE DKG identity gossip closed before all {n} identities collected"
                            ));
                        }
                    }
                },
                _ = self.clock.sleep(POLL) => {
                    // Timeout with no new identity: re-announce so peers that
                    // registered the round late still receive us. Bound the wait.
                    idle_ticks += 1;
                    if idle_ticks > 120 {
                        return Err(eyre::eyre!(
                            "TEE DKG identity exchange timed out: collected {}/{n}",
                            collected.ids.len()
                        ));
                    }
                    delivery::retry_delivery(&mut self.sender, &mut self.delivery);
                },
            }
        }
        Ok(request.announcements(collected.ids))
    }

    fn handle_identity_frame(
        &mut self,
        frame: IdentityFrame<'_>,
        collected: &mut IdentityCollection,
    ) -> eyre::Result<()> {
        match frame.bytes.first().copied() {
            Some(DKG_ENV_IDENTITY) => {
                if let Some((bls, enc, signature)) = parse_identity(&frame.bytes[1..]) {
                    self.accept_identity(
                        frame.from,
                        LocalIdentityAnnouncement {
                            bls,
                            enc,
                            signature,
                        },
                        collected,
                    )?;
                }
            }
            Some(DKG_ENV_CEREMONY) => {
                if let Ok(msg) = DkgWireMessage::from_bytes(&frame.bytes[1..]) {
                    self.buffered.push_back((frame.from.encode().to_vec(), msg));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn accept_identity(
        &mut self,
        from: bls12381::PublicKey,
        announcement: LocalIdentityAnnouncement,
        collected: &mut IdentityCollection,
    ) -> eyre::Result<()> {
        if collected.require_scoped && announcement.signature.is_empty() {
            return Ok(());
        }
        if !collected.require_scoped && !announcement.signature.is_empty() {
            self.retain_early_signed_identity(&announcement, collected.expected_count)?;
        }
        self.routing.insert(announcement.bls.clone(), from);
        collected
            .ids
            .insert(announcement.bls, (announcement.enc, announcement.signature));
        Ok(())
    }

    fn retain_early_signed_identity(
        &mut self,
        announcement: &LocalIdentityAnnouncement,
        n: usize,
    ) -> eyre::Result<()> {
        if !self.early_signed_identities.contains_key(&announcement.bls)
            && self.early_signed_identities.len() >= n
        {
            return Err(eyre::eyre!(
                "TEE DKG early signed identity budget exceeded: {n}"
            ));
        }
        self.early_signed_identities.insert(
            announcement.bls.clone(),
            (announcement.enc, announcement.signature.clone()),
        );
        Ok(())
    }
}

/// Parse an identity announcement body
/// `bls_len(u32 BE) || bls || enc(32) || sig_len(u32 BE) || sig`.
fn parse_identity(body: &[u8]) -> Option<(Vec<u8>, [u8; 32], Vec<u8>)> {
    if body.len() < 4 {
        return None;
    }
    let bls_len = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let enc_start = 4usize.checked_add(bls_len)?;
    let enc_end = enc_start.checked_add(32)?;
    let sig_len_end = enc_end.checked_add(4)?;
    if body.len() < sig_len_end {
        return None;
    }
    let sig_len = u32::from_be_bytes([
        body[enc_end],
        body[enc_end + 1],
        body[enc_end + 2],
        body[enc_end + 3],
    ]) as usize;
    let sig_end = sig_len_end.checked_add(sig_len)?;
    if sig_end != body.len() {
        return None;
    }
    let bls = body[4..enc_start].to_vec();
    let mut enc = [0u8; 32];
    enc.copy_from_slice(&body[enc_start..enc_end]);
    let sig = body[sig_len_end..sig_end].to_vec();
    Some((bls, enc, sig))
}
