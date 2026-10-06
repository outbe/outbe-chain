use super::*;

#[derive(Debug)]
pub(super) struct PendingDelivery {
    envelope: Vec<u8>,
    broadcast: bool,
    recipients: BTreeSet<bls12381::PublicKey>,
    acknowledged: BTreeSet<bls12381::PublicKey>,
    retry_in_ticks: u32,
    retry_every_ticks: u32,
}

/// Per-message delivery state. It is intentionally process-local. The enclosing
/// startup ceremony has one existing deadline. A node restart retries a failed
/// startup. P2P sender identities authenticate receipts.
#[derive(Debug)]
pub(super) struct DeliveryTracker {
    scope: B256,
    allowed_peers: BTreeSet<bls12381::PublicKey>,
    known_peers: BTreeSet<bls12381::PublicKey>,
    pub(super) pending: BTreeMap<B256, PendingDelivery>,
    pending_bytes: usize,
}

impl DeliveryTracker {
    fn message_id(&self, payload: &[u8]) -> B256 {
        let mut preimage = Vec::with_capacity(DELIVERY_ID_DOMAIN.len() + 32 + payload.len());
        preimage.extend_from_slice(DELIVERY_ID_DOMAIN);
        preimage.extend_from_slice(self.scope.as_slice());
        preimage.extend_from_slice(payload);
        keccak256(preimage)
    }

    pub(super) fn envelope(
        &mut self,
        recipients: Recipients<bls12381::PublicKey>,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, CeremonyError> {
        let id = self.message_id(&payload);
        if let Some(existing) = self.pending.get_mut(&id) {
            match recipients {
                Recipients::All => existing.broadcast = true,
                Recipients::One(peer) => {
                    existing.recipients.insert(peer);
                }
                _ => {
                    return Err(CeremonyError::Delivery(
                        "unsupported recipient mode for tracked delivery".to_string(),
                    ));
                }
            }
            return Ok(existing.envelope.clone());
        }
        let (broadcast, recipients) = match recipients {
            Recipients::All => (true, BTreeSet::new()),
            Recipients::One(peer) => (false, [peer].into_iter().collect()),
            _ => {
                return Err(CeremonyError::Delivery(
                    "unsupported recipient mode for tracked delivery".to_string(),
                ));
            }
        };
        let mut envelope = Vec::with_capacity(1 + DELIVERY_ID_LEN + payload.len());
        envelope.push(DELIVERY_DATA);
        envelope.extend_from_slice(id.as_slice());
        envelope.extend_from_slice(&payload);
        if self.pending.len() >= DELIVERY_MAX_PENDING_MESSAGES
            || self.pending_bytes.saturating_add(envelope.len()) > DELIVERY_MAX_PENDING_BYTES
        {
            return Err(CeremonyError::Delivery(format!(
                "pending delivery budget exceeded: messages={}, bytes={}",
                self.pending.len(),
                self.pending_bytes
            )));
        }
        self.pending_bytes += envelope.len();
        self.pending.insert(
            id,
            PendingDelivery {
                envelope: envelope.clone(),
                broadcast,
                recipients,
                acknowledged: BTreeSet::new(),
                retry_in_ticks: DELIVERY_INITIAL_RETRY_TICKS,
                retry_every_ticks: DELIVERY_INITIAL_RETRY_TICKS,
            },
        );
        Ok(envelope)
    }

    pub(super) fn observe_peer(&mut self, peer: bls12381::PublicKey) -> bool {
        if self.allowed_peers.contains(&peer) {
            self.known_peers.insert(peer);
            true
        } else {
            false
        }
    }

    pub(super) fn acknowledge(&mut self, peer: &bls12381::PublicKey, id: B256) {
        let complete = if let Some(pending) = self.pending.get_mut(&id) {
            pending.acknowledged.insert(peer.clone());
            if pending.broadcast {
                self.known_peers.len() >= self.allowed_peers.len()
                    && self
                        .known_peers
                        .iter()
                        .all(|known| pending.acknowledged.contains(known))
            } else {
                pending
                    .recipients
                    .iter()
                    .all(|expected| pending.acknowledged.contains(expected))
            }
        } else {
            false
        };
        if complete {
            if let Some(removed) = self.pending.remove(&id) {
                self.pending_bytes = self.pending_bytes.saturating_sub(removed.envelope.len());
            }
        }
    }

    pub(super) fn retry_batch(&mut self) -> Vec<(Recipients<bls12381::PublicKey>, Vec<u8>)> {
        let mut retry = Vec::new();
        for pending in self.pending.values_mut() {
            if pending.retry_in_ticks > 1 {
                pending.retry_in_ticks -= 1;
                continue;
            }
            if pending.broadcast && self.known_peers.len() >= self.allowed_peers.len() {
                for peer in self.known_peers.difference(&pending.acknowledged) {
                    retry.push((Recipients::One(peer.clone()), pending.envelope.clone()));
                }
            } else if pending.broadcast {
                retry.push((Recipients::All, pending.envelope.clone()));
            } else {
                for peer in pending.recipients.difference(&pending.acknowledged) {
                    retry.push((Recipients::One(peer.clone()), pending.envelope.clone()));
                }
            }
            pending.retry_every_ticks = pending
                .retry_every_ticks
                .saturating_mul(2)
                .min(DELIVERY_MAX_RETRY_TICKS);
            pending.retry_in_ticks = pending.retry_every_ticks;
        }
        retry
    }

    pub(super) fn decode<'a>(&self, bytes: &'a [u8]) -> Option<(u8, B256, &'a [u8])> {
        if bytes.len() < 1 + DELIVERY_ID_LEN {
            return None;
        }
        let tag = bytes[0];
        if tag != DELIVERY_DATA && tag != DELIVERY_ACK {
            return None;
        }
        let id = B256::from_slice(&bytes[1..1 + DELIVERY_ID_LEN]);
        let payload = &bytes[1 + DELIVERY_ID_LEN..];
        match tag {
            DELIVERY_DATA if self.message_id(payload) == id => Some((tag, id, payload)),
            DELIVERY_ACK if payload.is_empty() => Some((tag, id, payload)),
            _ => None,
        }
    }
}

pub(super) fn new_delivery_tracker(
    scope: B256,
    allowed_peers: BTreeSet<bls12381::PublicKey>,
) -> DeliveryTracker {
    DeliveryTracker {
        scope,
        allowed_peers,
        known_peers: BTreeSet::new(),
        pending: BTreeMap::new(),
        pending_bytes: 0,
    }
}

pub(super) fn ack_envelope(id: B256) -> Vec<u8> {
    let mut ack = Vec::with_capacity(1 + DELIVERY_ID_LEN);
    ack.push(DELIVERY_ACK);
    ack.extend_from_slice(id.as_slice());
    ack
}

/// Authenticate the transport peer before decoding or recording its receipt.
/// The caller sends the ACK immediately before interpreting any data payload.
pub(super) fn receive_delivery<'a>(
    tracker: &mut DeliveryTracker,
    from: &bls12381::PublicKey,
    raw: &'a [u8],
) -> Option<(B256, &'a [u8])> {
    if !tracker.observe_peer(from.clone()) {
        return None;
    }
    match tracker.decode(raw) {
        Some((DELIVERY_DATA, id, payload)) => Some((id, payload)),
        Some((DELIVERY_ACK, id, _)) => {
            tracker.acknowledge(from, id);
            None
        }
        _ => None,
    }
}

pub(super) fn retry_delivery<S: P2pSender<PublicKey = bls12381::PublicKey>>(
    sender: &mut S,
    tracker: &mut DeliveryTracker,
) {
    for (recipients, bytes) in tracker.retry_batch() {
        let _ = sender.send(recipients, bytes, true);
    }
}
