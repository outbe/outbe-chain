use super::*;

/// Owned Commonware channel, runtime clock and permitted remote delivery peers
/// for one startup ceremony. The ceremony supplies its own delivery scope.
pub struct StartupGossipTransport<S, R, C> {
    pub sender: S,
    pub receiver: R,
    pub clock: C,
    pub allowed_remote_peers: BTreeSet<bls12381::PublicKey>,
}

/// Adapts the consensus P2P channel (commonware `Sender`/`Receiver`) to the
/// [`BootstrapGossip`] surface the coordination needs. Messages are opaque bytes.
pub struct CommonwareBootstrapGossip<S, R, C> {
    pub sender: S,
    pub receiver: R,
    pub(super) clock: C,
    pub(super) delivery: DeliveryTracker,
}

impl<S, R, C> BootstrapGossip for CommonwareBootstrapGossip<S, R, C>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError> {
        let envelope = self.delivery.envelope(Recipients::All, bytes)?;
        let _ = self.sender.send(Recipients::All, envelope, true);
        Ok(())
    }

    async fn recv(&mut self) -> Option<Vec<u8>> {
        const RETRY_POLL: std::time::Duration = std::time::Duration::from_millis(750);
        loop {
            commonware_macros::select! {
                recv = self.receiver.recv() => {
                    return match recv {
                        Ok((from, raw)) => {
                            let Some((id, payload)) = receive_delivery(&mut self.delivery, &from, raw.as_ref()) else { continue; };
                            let _ = self.sender.send(Recipients::One(from), ack_envelope(id), true);
                            Some(payload.to_vec())
                        }
                        Err(_) => None,
                    };
                },
                _ = self.clock.sleep(RETRY_POLL) => {
                    delivery::retry_delivery(&mut self.sender, &mut self.delivery);
                },
            }
        }
    }
}

/// Adapts the consensus P2P channel to the TEE-DKG [`DkgGossip`] surface, and runs
/// the pre-ceremony enclave-identity exchange.
///
/// Two message kinds share the channel. A 1-byte envelope tag tells them apart:
/// ceremony messages ([`DkgWireMessage`]) and identity announcements
/// (`bls_len(u32 BE) || bls || enc(32) || sig_len(u32 BE) || sig`). The
/// ceremony addresses dealer->player bundles by the recipient's *enclave* BLS
/// key, but P2P routes by the *consensus* BLS key.
/// Thus the identity exchange builds a `tee_bls -> consensus_pubkey` routing map
/// from the authenticated sender of each identity message. Sends use this map
/// for their address. If the channel broadcast addressed bundles instead, every
/// non-recipient enclave would fail to open the share (it is sealed to one
/// recipient) and abort the ceremony.
pub struct CommonwareDkgGossip<S, R, C> {
    pub(super) sender: S,
    pub(super) receiver: R,
    pub(super) clock: C,
    /// `tee_bls -> consensus P2P pubkey` for addressed ceremony sends.
    pub(super) routing: BTreeMap<Vec<u8>, bls12381::PublicKey>,
    /// Ceremony messages received during the identity-exchange phase. `recv`
    /// replays them before it reads new ones, so the phase race loses nothing.
    pub(super) buffered: VecDeque<(Vec<u8>, DkgWireMessage)>,
    /// Signed announcements can arrive during preliminary BLS discovery. Once
    /// ACKed, they must survive the phase transition. The sender may stop retrying.
    /// Bounded by the expected participant count of this startup exchange.
    pub(super) early_signed_identities: BTreeMap<Vec<u8>, ([u8; 32], Vec<u8>)>,
    /// Typed, bounded delivery state. Transport acknowledgements suppress
    /// retries per message and peer. The enclosing startup timeout remains the
    /// ceremony deadline.
    pub(super) delivery: DeliveryTracker,
}

impl<S, R, C> CommonwareDkgGossip<S, R, C>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    pub fn new(transport: StartupGossipTransport<S, R, C>, scope: B256) -> Self {
        Self {
            sender: transport.sender,
            receiver: transport.receiver,
            clock: transport.clock,
            routing: BTreeMap::new(),
            buffered: VecDeque::new(),
            early_signed_identities: BTreeMap::new(),
            delivery: new_delivery_tracker(scope, transport.allowed_remote_peers),
        }
    }

    fn send_ceremony(
        &mut self,
        recipients: Recipients<bls12381::PublicKey>,
        msg: &DkgWireMessage,
    ) -> Result<(), CeremonyError> {
        let mut env = Vec::with_capacity(1 + 8);
        env.push(DKG_ENV_CEREMONY);
        env.extend_from_slice(&msg.to_bytes());
        let envelope = self.delivery.envelope(recipients.clone(), env)?;
        let _ = self.sender.send(recipients, envelope, true);
        Ok(())
    }
}

impl<S, R, C> DkgGossip for CommonwareDkgGossip<S, R, C>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    C: Clock,
{
    async fn send(&mut self, to: &[u8], msg: DkgWireMessage) -> Result<(), CeremonyError> {
        match self.routing.get(to).cloned() {
            Some(peer) => self.send_ceremony(Recipients::One(peer), &msg)?,
            // No route (should not happen after the identity exchange): broadcast so the
            // recipient still receives it. Non-recipients ignore foreign shares.
            None => self.send_ceremony(Recipients::All, &msg)?,
        }
        Ok(())
    }

    async fn broadcast(&mut self, msg: DkgWireMessage) -> Result<(), CeremonyError> {
        self.send_ceremony(Recipients::All, &msg)?;
        Ok(())
    }

    async fn recv(&mut self) -> Option<(Vec<u8>, DkgWireMessage)> {
        if let Some(buffered) = self.buffered.pop_front() {
            return Some(buffered);
        }
        const RETRY_POLL: std::time::Duration = std::time::Duration::from_millis(750);
        loop {
            commonware_macros::select! {
                recv = self.receiver.recv() => {
                    let (from, raw) = recv.ok()?;
                    let Some((id, bytes)) = receive_delivery(&mut self.delivery, &from, raw.as_ref()) else { continue; };
                    let _ = self.sender.send(Recipients::One(from.clone()), ack_envelope(id), true);
                    match bytes.first().copied() {
                        Some(DKG_ENV_CEREMONY) => match DkgWireMessage::from_bytes(&bytes[1..]) {
                            Ok(msg) => return Some((from.encode().to_vec(), msg)),
                            Err(_) => continue,
                        },
                        // Late identity announcement (a peer still in its exchange phase).
                        // Ignore it and keep reading.
                        _ => continue,
                    }
                },
                _ = self.clock.sleep(RETRY_POLL) => {
                    delivery::retry_delivery(&mut self.sender, &mut self.delivery);
                },
            }
        }
    }
}
