//! Consensus-thread TEE bootstrap: run the one-time committee coordination at
//! startup (exactly like the consensus DKG), assemble the `TeeBootstrapPayload`,
//! and hand it to the payload builder via the bridge so the **block-1** proposer
//! injects it (slice 5.1). `committee_snapshot_block` is the fixed block 1 — the
//! known injection target, mirroring how `BoundaryOutcome` lands at block 1 — so
//! there is no run-time block-number ambiguity.
//!
//! The secret operations stay in the enclave; this glue only adapts the
//! consensus P2P channel to [`BootstrapGossip`] and signs the payload with the
//! validator's EVM key.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use alloy_primitives::{keccak256, Address, B256};
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381;
use commonware_p2p::{Receiver as P2pReceiver, Recipients, Sender as P2pSender};

use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::tee_bootstrap::TeeBootstrapPayload;
use outbe_tee::bootstrap::{run_tee_bootstrap_coordination, BootstrapGossip, BootstrapParams};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::tee_dkg::{
    run_tee_dkg_ceremony, CeremonyCoordinator, CeremonyError, DkgGossip, DkgWireMessage,
};
use outbe_tee::{EnclaveClient, QuotePolicy};

/// The fixed block the one-time TEE bootstrap targets (the first non-genesis
/// block, where `BoundaryOutcome` activates the genesis committee).
const TEE_BOOTSTRAP_BLOCK: u64 = 1;

/// Adapts the consensus P2P channel (commonware `Sender`/`Receiver`) to the
/// [`BootstrapGossip`] surface the coordination needs. Messages are opaque bytes.
pub struct CommonwareBootstrapGossip<S, R> {
    pub sender: S,
    pub receiver: R,
}

impl<S, R> BootstrapGossip for CommonwareBootstrapGossip<S, R>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError> {
        // Mirror the DKG actor: `send` returns the accepting peers; an empty Vec
        // is benign backpressure, not a hard failure (peers also receive via the
        // committee's own broadcasts).
        let _ = self.sender.send(Recipients::All, bytes, true);
        Ok(())
    }

    async fn recv(&mut self) -> Option<Vec<u8>> {
        match self.receiver.recv().await {
            Ok((_from, raw)) => Some(raw.as_ref().to_vec()),
            Err(_) => None,
        }
    }
}

/// Envelope tag for a ceremony [`DkgWireMessage`] on the TEE-DKG channel.
const DKG_ENV_CEREMONY: u8 = 0x00;
/// Envelope tag for an enclave-identity announcement on the TEE-DKG channel.
const DKG_ENV_IDENTITY: u8 = 0x01;

/// Adapts the consensus P2P channel to the TEE-DKG [`DkgGossip`] surface, and runs
/// the pre-ceremony enclave-identity exchange.
///
/// Two message kinds share the channel, distinguished by a 1-byte envelope tag:
/// ceremony messages ([`DkgWireMessage`]) and identity announcements
/// (`tee_bls || dkg_enc`). The ceremony addresses dealer→player bundles by the
/// recipient's *enclave* BLS key, but P2P routes by the *consensus* BLS key, so a
/// `tee_bls -> consensus_pubkey` routing map is built during identity exchange
/// (from the authenticated sender of each identity message) and used to address
/// sends. Broadcasting addressed bundles instead would make every non-recipient
/// enclave fail to open the share (it is sealed to one recipient) and abort the
/// ceremony.
pub struct CommonwareDkgGossip<S, R> {
    sender: S,
    receiver: R,
    /// `tee_bls -> consensus P2P pubkey` for addressed ceremony sends.
    routing: BTreeMap<Vec<u8>, bls12381::PublicKey>,
    /// Ceremony messages received during the identity-exchange phase, replayed
    /// before reading new ones so the phase race loses nothing.
    buffered: VecDeque<(Vec<u8>, DkgWireMessage)>,
}

impl<S, R> CommonwareDkgGossip<S, R>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    pub fn new(sender: S, receiver: R) -> Self {
        Self {
            sender,
            receiver,
            routing: BTreeMap::new(),
            buffered: VecDeque::new(),
        }
    }

    fn send_ceremony(&mut self, recipients: Recipients<bls12381::PublicKey>, msg: &DkgWireMessage) {
        let mut env = Vec::with_capacity(1 + 8);
        env.push(DKG_ENV_CEREMONY);
        env.extend_from_slice(&msg.to_bytes());
        let _ = self.sender.send(recipients, env, true);
    }

    /// Announce this enclave's `(tee_bls, dkg_enc)` identity and collect all `n`
    /// participants' identities, buffering any ceremony messages that arrive
    /// early and recording the `tee_bls -> consensus_pubkey` routing. Returns the
    /// identities sorted canonically by `tee_bls` (so every node derives the same
    /// ceremony id and participant order).
    pub async fn exchange_identities(
        &mut self,
        my_bls: Vec<u8>,
        my_enc: [u8; 32],
        n: usize,
    ) -> eyre::Result<Vec<(Vec<u8>, [u8; 32])>> {
        let mut ids: BTreeMap<Vec<u8>, [u8; 32]> = BTreeMap::new();
        ids.insert(my_bls.clone(), my_enc);

        let mut env = vec![DKG_ENV_IDENTITY];
        env.extend_from_slice(&(my_bls.len() as u32).to_be_bytes());
        env.extend_from_slice(&my_bls);
        env.extend_from_slice(&my_enc);
        let _ = self.sender.send(Recipients::All, env.clone(), true);

        // Re-broadcast our identity periodically until every peer's identity is
        // collected. The muxed sub-channel drops messages addressed to a round a
        // peer has not yet registered, so a node that announces before its peers
        // register would otherwise be lost and the exchange would hang. Retrying
        // makes the exchange robust to that registration race on every round.
        let mut idle_ticks = 0u32;
        while ids.len() < n {
            match tokio::time::timeout(std::time::Duration::from_millis(750), self.receiver.recv())
                .await
            {
                Ok(Ok((from, raw))) => {
                    let bytes = raw.as_ref();
                    match bytes.first().copied() {
                        Some(DKG_ENV_IDENTITY) => {
                            if let Some((bls, enc)) = parse_identity(&bytes[1..]) {
                                self.routing.insert(bls.clone(), from);
                                ids.insert(bls, enc);
                            }
                        }
                        Some(DKG_ENV_CEREMONY) => {
                            if let Ok(msg) = DkgWireMessage::from_bytes(&bytes[1..]) {
                                self.buffered.push_back((from.encode().to_vec(), msg));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Err(_)) => {
                    return Err(eyre::eyre!(
                        "TEE DKG identity gossip closed before all {n} identities collected"
                    ));
                }
                Err(_) => {
                    // Timeout with no new identity: re-announce so peers that
                    // registered the round late still receive us. Bound the wait.
                    idle_ticks += 1;
                    if idle_ticks > 120 {
                        return Err(eyre::eyre!(
                            "TEE DKG identity exchange timed out: collected {}/{n}",
                            ids.len()
                        ));
                    }
                    let _ = self.sender.send(Recipients::All, env.clone(), true);
                }
            }
        }
        Ok(ids.into_iter().collect())
    }
}

impl<S, R> DkgGossip for CommonwareDkgGossip<S, R>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    async fn send(&mut self, to: &[u8], msg: DkgWireMessage) -> Result<(), CeremonyError> {
        match self.routing.get(to).cloned() {
            Some(peer) => self.send_ceremony(Recipients::One(peer), &msg),
            // No route (should not happen post identity-exchange): broadcast so the
            // recipient still receives it; non-recipients ignore foreign shares.
            None => self.send_ceremony(Recipients::All, &msg),
        }
        Ok(())
    }

    async fn broadcast(&mut self, msg: DkgWireMessage) -> Result<(), CeremonyError> {
        self.send_ceremony(Recipients::All, &msg);
        Ok(())
    }

    async fn recv(&mut self) -> Option<(Vec<u8>, DkgWireMessage)> {
        if let Some(buffered) = self.buffered.pop_front() {
            return Some(buffered);
        }
        loop {
            let (from, raw) = self.receiver.recv().await.ok()?;
            let bytes = raw.as_ref();
            match bytes.first().copied() {
                Some(DKG_ENV_CEREMONY) => match DkgWireMessage::from_bytes(&bytes[1..]) {
                    Ok(msg) => return Some((from.encode().to_vec(), msg)),
                    Err(_) => continue,
                },
                // Late identity announcement (a peer still in its exchange phase):
                // ignore and keep reading.
                _ => continue,
            }
        }
    }
}

/// Parse an identity announcement body `bls_len(u32 BE) || bls || enc(32)`.
fn parse_identity(body: &[u8]) -> Option<(Vec<u8>, [u8; 32])> {
    if body.len() < 4 {
        return None;
    }
    let bls_len = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let enc_start = 4usize.checked_add(bls_len)?;
    let end = enc_start.checked_add(32)?;
    if end != body.len() {
        return None;
    }
    let bls = body[4..enc_start].to_vec();
    let mut enc = [0u8; 32];
    enc.copy_from_slice(&body[enc_start..end]);
    Some((bls, enc))
}

/// Deterministic ceremony id, identical on every node given the same chain and
/// sorted participant set: `keccak256(chain_id || round || tee_bls_0 || …)`.
fn compute_ceremony_id(chain_id: B256, round: u64, identities: &[(Vec<u8>, [u8; 32])]) -> B256 {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(chain_id.as_slice());
    preimage.extend_from_slice(&round.to_be_bytes());
    for (bls, _) in identities {
        preimage.extend_from_slice(&(bls.len() as u32).to_be_bytes());
        preimage.extend_from_slice(bls);
    }
    keccak256(&preimage)
}

/// Run the one-time TEE DKG ceremony at startup and return the **shared offer
/// public key** derived from the group threshold signature (Seam F). Connects
/// this node's enclave, exchanges enclave identities across the committee,
/// drives the dealer/player ceremony + the offer-key partial-signature round
/// entirely through the enclave seams, and returns the byte-identical
/// `tribute_offer_public` every honest node derives. The offer *secret* never leaves the
/// enclave; it is stored resident there and used to decrypt offers.
///
/// `n` is the committee size; `chain_id`/`tribute_offer_epoch` bind the derived offer
/// key. Runs before [`run_tee_bootstrap_at_startup`], whose payload registers the
/// returned key on-chain at block 1.
/// Build the host connect [`QuotePolicy`] from the genesis `teePolicy`:
/// strict (measurement allowlist + DCAP signature) for a real gramine-sgx quote,
/// with an unattested fallback so the gramine-direct dev box still connects (the
/// caller logs it as not-confidential). An empty genesis policy → `dev_accept_any`.
/// The deterministic on-chain Phase-3b measurement gate enforces the policy
/// regardless; this is the host-side defense-in-depth at connect time.
pub fn quote_policy_from_tee_policy(
    policy: &outbe_primitives::tee_bootstrap::TeePolicy,
) -> QuotePolicy {
    if policy.is_empty() {
        QuotePolicy::dev_accept_any()
    } else {
        QuotePolicy::from_genesis_strict(
            policy.allowed_mrenclave.clone(),
            policy.allowed_mrsigner.clone(),
            policy.min_isv_svn,
        )
    }
}

pub async fn run_tee_dkg_at_startup<S, R>(
    enclave_socket: &Path,
    n: usize,
    chain_id: B256,
    tribute_offer_epoch: u64,
    connect_policy: &QuotePolicy,
    sender: S,
    receiver: R,
) -> eyre::Result<[u8; 32]>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    let endpoint = enclave_socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let mut client = EnclaveClient::connect_endpoint(endpoint, connect_policy)
        .map_err(|e| eyre::eyre!("TEE DKG enclave connect failed: {e}"))?;

    let (my_bls, my_enc) = match client
        .request(&EnclaveRequest::GetPublicKeys)
        .map_err(|e| eyre::eyre!("TEE DKG GetPublicKeys failed: {e}"))?
    {
        EnclaveResponse::PublicKeys {
            tee_bls_pub,
            dkg_enc_pub,
            ..
        } => (tee_bls_pub, dkg_enc_pub),
        other => return Err(eyre::eyre!("unexpected GetPublicKeys response: {other:?}")),
    };

    let mut gossip = CommonwareDkgGossip::new(sender, receiver);
    let identities = gossip
        .exchange_identities(my_bls.clone(), my_enc, n)
        .await?;
    let ceremony_id = compute_ceremony_id(chain_id, 0, &identities);
    let coord = CeremonyCoordinator::new(ceremony_id, 0, my_bls, identities);

    let outcome = run_tee_dkg_ceremony(
        &coord,
        &mut client,
        &mut gossip,
        n,
        chain_id,
        tribute_offer_epoch,
    )
    .await
    .map_err(|e| eyre::eyre!("TEE DKG ceremony failed: {e}"))?;

    Ok(outcome.tribute_offer_public)
}

/// Run the one-time TEE bootstrap coordination at startup and return the signed
/// payload for the bridge. Connects this node's enclave, fetches its attested
/// registration, coordinates registrations + EVM signatures across `committee`,
/// and returns the byte-identical payload every honest node produces.
///
/// `my_validator` is this node's EVM address (the registration's validator and
/// the EVM signer's address). `tribute_offer_public_key` is the shared offer key
/// derived by the TEE DKG ([`run_tee_dkg_at_startup`]); it is registered on-chain
/// at block 1 so clients encrypt offers to it.
// Startup glue with several independent, clearly-named consensus inputs (socket,
// validator, committee, offer key, policy, signer, P2P endpoints). Bundling them
// into a struct would not add clarity at the single call site in `stack.rs`.
#[allow(clippy::too_many_arguments)]
pub async fn run_tee_bootstrap_at_startup<S, R>(
    enclave_socket: &Path,
    my_validator: Address,
    committee: BTreeSet<Address>,
    tribute_offer_public_key: B256,
    policy: outbe_primitives::tee_bootstrap::TeePolicy,
    evm_signer: &OutbeEvmSigner,
    sender: S,
    receiver: R,
) -> eyre::Result<TeeBootstrapPayload>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    // Fail fast if the validator's EVM signer cannot sign — otherwise the
    // coordination would emit an unverifiable signature.
    evm_signer
        .sign_hash(&B256::ZERO)
        .map_err(|e| eyre::eyre!("validator EVM signer cannot sign bootstrap: {e}"))?;

    let endpoint = enclave_socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let connect_policy = quote_policy_from_tee_policy(&policy);
    let client = EnclaveClient::connect_endpoint(endpoint, &connect_policy)
        .map_err(|e| eyre::eyre!("TEE bootstrap enclave connect failed: {e}"))?;
    let registration = client.enclave_registration(my_validator);

    let params = BootstrapParams {
        // The shared offer key derived by the TEE DKG (Seam F). Every honest node
        // derives the byte-identical key, so all produce the same payload and
        // register the same key at block 1; clients encrypt offers to it.
        tribute_offer_public_key,
        // Genesis attestation allowlist: parsed from
        // `config.teePolicy` and passed in by the caller. `payload.policy_hash`
        // derives from it and must equal the genesis-seeded `TeeRegistry.policy_hash`
        // (slot 2); a default (empty) policy => ZERO-ish hash, and the handler
        // skips enforcement only when slot 2 is itself unseeded (ZERO).
        policy,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        dkg_transcript_hash: B256::ZERO,
        committee_snapshot_block: TEE_BOOTSTRAP_BLOCK,
        committee_snapshot_hash: B256::ZERO,
    };

    let mut gossip = CommonwareBootstrapGossip { sender, receiver };
    let payload = run_tee_bootstrap_coordination(
        registration,
        &params,
        &committee,
        // Pre-validated above, so the fallback is never reached for a valid signer.
        |hash| evm_signer.sign_hash(hash).unwrap_or([0u8; 65]),
        &mut gossip,
    )
    .await
    .map_err(|e| eyre::eyre!("TEE bootstrap coordination failed: {e}"))?;

    Ok(payload)
}

/// NEWCOMER: a joining/keyless committee member obtains the resident offer
/// key via key-handoff over the P2P channel. Connects to this node's enclave,
/// broadcasts its attested quote, and ingests the first reply that verifies against
/// the on-chain `expected_tribute_offer_public` (1-of-n — a malicious server cannot
/// install a wrong key). Returns once the enclave holds the offer key; the caller
/// bounds the wait (fail-fast timeout).
#[allow(clippy::too_many_arguments)]
pub async fn run_tee_handoff_join<S, R>(
    enclave_socket: &Path,
    expected_tribute_offer_public: B256,
    chain_id: B256,
    tribute_offer_epoch: u64,
    min_confirmations: usize,
    connect_policy: &QuotePolicy,
    sender: S,
    receiver: R,
) -> eyre::Result<()>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    let endpoint = enclave_socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let mut client = EnclaveClient::connect_endpoint(endpoint, connect_policy)
        .map_err(|e| eyre::eyre!("TEE handoff enclave connect failed: {e}"))?;
    let my_quote = client.quote().clone();
    let mut gossip = CommonwareHandoffGossip { sender, receiver };
    outbe_tee::run_handoff_as_newcomer(
        &mut client,
        &mut gossip,
        my_quote,
        expected_tribute_offer_public.0,
        chain_id,
        tribute_offer_epoch,
        min_confirmations,
    )
    .await
    .map_err(|e| eyre::eyre!("TEE key-handoff failed: {e}"))
}

/// Adapts the consensus P2P channel to the key-handoff [`outbe_tee::HandoffGossip`] surface
/// — `recv` yields the sender's encoded consensus key so the newcomer can require a
/// QUORUM of distinct responders.
struct CommonwareHandoffGossip<S, R> {
    sender: S,
    receiver: R,
}

impl<S, R> outbe_tee::HandoffGossip for CommonwareHandoffGossip<S, R>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
{
    async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError> {
        let _ = self.sender.send(Recipients::All, bytes, true);
        Ok(())
    }

    async fn recv(&mut self) -> outbe_tee::HandoffEvent {
        // Bound the wait so the newcomer driver gets a periodic idle tick and can
        // re-broadcast a lost request (mirrors the DKG identity-exchange re-announce,
        // which relies on the same `tokio::time::timeout` cancel-safety on this
        // receiver). On timeout the in-flight `recv` future is dropped; a buffered
        // message is not lost (same guarantee the DKG path already depends on).
        const POLL: std::time::Duration = std::time::Duration::from_millis(750);
        match tokio::time::timeout(POLL, self.receiver.recv()).await {
            Ok(Ok((from, raw))) => outbe_tee::HandoffEvent::Message {
                peer: from.encode().to_vec(),
                bytes: raw.as_ref().to_vec(),
            },
            Ok(Err(_)) => outbe_tee::HandoffEvent::Closed,
            Err(_) => outbe_tee::HandoffEvent::Idle,
        }
    }
}

/// An [`EnclaveChannel`] that lazily (re)connects to the enclave and retries once on
/// a transport failure, so the long-lived handoff responder recovers from an enclave
/// restart instead of going permanently dead on a dropped connection. A
/// request first tries the cached connection; on any transport error it drops it,
/// reconnects, and retries once. If the reconnect itself fails the error propagates,
/// and `answer_handoff_request` turns it into "no reply" (the responder stays alive
/// and reconnects on the next request).
struct ReconnectingEnclave<'a> {
    endpoint: &'a str,
    policy: &'a QuotePolicy,
    client: Option<EnclaveClient>,
}

impl<'a> ReconnectingEnclave<'a> {
    fn new(endpoint: &'a str, policy: &'a QuotePolicy) -> Self {
        Self {
            endpoint,
            policy,
            client: None,
        }
    }
}

impl outbe_tee::EnclaveChannel for ReconnectingEnclave<'_> {
    fn request(
        &mut self,
        req: &EnclaveRequest,
    ) -> Result<EnclaveResponse, outbe_tee::TransportError> {
        // Try the cached connection first; drop it on any transport error.
        if let Some(client) = self.client.as_mut() {
            match client.request(req) {
                Ok(resp) => return Ok(resp),
                Err(_) => self.client = None,
            }
        }
        // (Re)connect and retry once.
        let mut fresh = EnclaveClient::connect_endpoint(self.endpoint, self.policy)?;
        let resp = fresh.request(req)?;
        self.client = Some(fresh);
        Ok(resp)
    }
}

/// SERVER: long-running responder for key-handoff requests. Connects to this
/// node's enclave, then for each request from an authorized active-set peer, verifies
/// the requester's quote and seals the resident group signature to its attested
/// X25519 key. A node without a resident key answers nothing (the enclave's
/// `SealTributeOfferHandoff` fails → no reply). Runs until the channel closes. The
/// enclave connection auto-reconnects (see [`ReconnectingEnclave`]) so an enclave
/// restart does not silently kill the responder.
pub async fn serve_tee_handoff<S, R, F>(
    enclave_socket: &Path,
    connect_policy: &QuotePolicy,
    verify_policy: QuotePolicy,
    authorized: Vec<bls12381::PublicKey>,
    registered_recipient: F,
    mut sender: S,
    mut receiver: R,
) -> eyre::Result<()>
where
    S: P2pSender<PublicKey = bls12381::PublicKey>,
    R: P2pReceiver<PublicKey = bls12381::PublicKey>,
    // Resolve a requester's on-chain registered `recipient_x25519` from its
    // consensus-key bytes, read at REQUEST time so a registration written
    // after genesis — including this validator's own `registerEnclave` re-submit —
    // is reflected; `None` for an unregistered validator (→ fallback-served).
    F: Fn(&[u8]) -> Option<B256>,
{
    let endpoint = enclave_socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    // Lazily (re)connecting enclave channel: the responder survives an enclave
    // restart instead of dying on a dropped connection. The first served
    // request establishes the connection.
    let mut client = ReconnectingEnclave::new(endpoint, connect_policy);
    // Anti-DoS: a per-requester cooldown so a flood of requests from one
    // peer cannot pin the enclave (each seal is an enclave round-trip). A legit
    // newcomer needs the key once; the cooldown still allows a retry if its first
    // reply was lost. `Instant` is monotonic + local — this is a side service, not a
    // consensus-visible path.
    const HANDOFF_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(3);
    let mut last_served: BTreeMap<Vec<u8>, std::time::Instant> = BTreeMap::new();
    while let Ok((from, raw)) = receiver.recv().await {
        // Authorize: only an active-set committee member may request the key. The
        // P2P layer authenticates `from` as the sender's consensus key.
        if !authorized.contains(&from) {
            continue;
        }
        let from_key = from.encode().to_vec();
        let now = std::time::Instant::now();
        if let Some(prev) = last_served.get(&from_key) {
            if now.duration_since(*prev) < HANDOFF_COOLDOWN {
                continue; // rate-limited
            }
        }
        // Registration binding: when the requester is registered on-chain, the resident
        // group signature is sealed ONLY to its registered `recipient_x25519`. This is
        // an on-chain identity check that substitutes for the not-yet-real attestation
        // check (under `dev_accept_any` the quote does not yet prove the key is enclave-
        // resident, so the on-chain registration is the trust anchor). Resolved at
        // request time from the latest state. An unregistered-but-attested requester is
        // still served (fallback) so a first-time joiner can onboard before registering.
        let expected_recipient = registered_recipient(&from_key);
        if let Some(reply) = outbe_tee::answer_handoff_request(
            &mut client,
            raw.as_ref(),
            &verify_policy,
            |attested| match expected_recipient {
                Some(expected) if expected != B256::ZERO => {
                    B256::from(attested.recipient_x25519) == expected
                }
                _ => true,
            },
        ) {
            last_served.insert(from_key, now);
            let _ = sender.send(Recipients::One(from), reply, true);
        }
    }
    Ok(())
}

/// Connect to the enclave and read the offer public key it currently advertises
/// (`GetPublicKeys.recipient_x25519_pub`) — the resident DKG/handed-off offer key,
/// or the dev fallback if the enclave has none. Used at existing-chain startup to
/// decide whether this node needs a key-handoff: its value differs from the
/// on-chain registered key exactly when the enclave lacks the offer key.
pub fn query_enclave_offer_public(
    enclave_socket: &Path,
    connect_policy: &QuotePolicy,
) -> eyre::Result<B256> {
    let endpoint = enclave_socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let mut client = EnclaveClient::connect_endpoint(endpoint, connect_policy)
        .map_err(|e| eyre::eyre!("TEE enclave connect failed: {e}"))?;
    match client
        .request(&EnclaveRequest::GetPublicKeys)
        .map_err(|e| eyre::eyre!("TEE GetPublicKeys failed: {e}"))?
    {
        EnclaveResponse::PublicKeys {
            recipient_x25519_pub,
            ..
        } => Ok(B256::from(recipient_x25519_pub)),
        other => Err(eyre::eyre!("unexpected GetPublicKeys response: {other:?}")),
    }
}

alloy_sol_types::sol! {
    /// The `TeeRegistry` (`0xEE0A`) mid-chain registration ABI. Mirrors
    /// the `#[contract_public]` signature in `outbe-teeregistry`'s precompile.
    function registerEnclave(
        uint256 recipientX25519,
        uint256 attestationPub,
        uint256 noiseStaticPub,
        uint256 mrenclave,
        uint256 mrsigner,
        uint16 isvSvn
    ) external returns (bool);
}

/// Gas cap for the `registerEnclave` call — a handful of per-validator storage
/// writes.
const REGISTER_ENCLAVE_GAS_LIMIT: u64 = 300_000;

/// reth's pool `minimal_protocol_basefee` floor (`alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE`
/// = 7 wei): a transaction whose fee cap is below this is rejected at pool insert,
/// regardless of origin (there is NO local-origin exemption). The registration tx's
/// `gas_price` must clear both this floor and the current block base fee.
const REGISTER_ENCLAVE_MIN_BASE_FEE: u128 = 7;

/// Build the `registerEnclave` tx `gas_price` from the latest block base fee: take
/// `max(base_fee, pool_floor)` so the tx is admitted (≥ the 7-wei pool floor) and
/// includable (≥ the current base fee), then double it for headroom against a base
/// fee rise between build and inclusion. The cost (`gas_limit × gas_price`) is
/// trivial — a few hundred-thousand × single-digit-to-gwei wei — against a funded
/// validator EOA. Returning a non-zero, base-fee-aware price is what makes the tx
/// actually land (a `gas_price = 0` legacy tx is rejected at pool insert).
fn register_enclave_gas_price(base_fee: u64) -> u128 {
    u128::from(base_fee)
        .max(REGISTER_ENCLAVE_MIN_BASE_FEE)
        .saturating_mul(2)
}

/// Submit this validator's on-chain enclave registration as a normal
/// EOA transaction, mirroring Secret Network's node-submitted `x/registration`.
///
/// Reads the enclave's current key material from its attested quote
/// (`recipient_x25519`, `attestation_pub`, `noise_static_pub`, `mrenclave`,
/// `mrsigner`, `isv_svn`), ABI-encodes `registerEnclave(...)`, signs it with this
/// validator's EVM key, and submits it to the local txpool. The `caller` recorded
/// on-chain is the validator's EVM signer address (the precompile keys the
/// registration by `caller`), so every validator registers itself — idempotent,
/// no cross-node coordination. Re-registration on restart refreshes the ephemeral
/// attestation/noise keys.
///
/// `base_fee` is the latest block's base fee (read at the call site from the header
/// provider); the tx `gas_price` is derived from it via [`register_enclave_gas_price`]
/// so the tx clears the pool's minimum-fee floor and the current base fee and is
/// actually included. The validator EOA pays a trivial fee (it is funded). Returns
/// the submitted transaction hash.
pub async fn submit_enclave_registration<Pool>(
    enclave_socket: &Path,
    connect_policy: &QuotePolicy,
    signer: &OutbeEvmSigner,
    pool: &Pool,
    provider: &dyn reth_ethereum::storage::StateProviderFactory,
    chain_id: u64,
    base_fee: u64,
) -> eyre::Result<B256>
where
    Pool: reth_transaction_pool::TransactionPool<
        Transaction = reth_transaction_pool::EthPooledTransaction,
    >,
{
    use alloy_primitives::{Bytes, TxKind, U256};
    use alloy_sol_types::SolCall as _;
    use reth_primitives_traits::SignedTransaction as _;

    let endpoint = enclave_socket
        .to_str()
        .ok_or_else(|| eyre::eyre!("TEE enclave endpoint is not valid UTF-8"))?;
    let client = EnclaveClient::connect_endpoint(endpoint, connect_policy)
        .map_err(|e| eyre::eyre!("TEE registration enclave connect failed: {e}"))?;

    // The connect-time quote carries every key the registry records.
    let (recipient_x25519, attestation_pub, noise_static_pub, mrenclave, mrsigner, isv_svn) =
        match client.quote() {
            EnclaveResponse::Quote {
                recipient_x25519_pub,
                attestation_pub,
                noise_static_pub,
                mrenclave,
                mrsigner,
                isv_svn,
                ..
            } => (
                *recipient_x25519_pub,
                *attestation_pub,
                *noise_static_pub,
                *mrenclave,
                *mrsigner,
                *isv_svn,
            ),
            other => {
                return Err(eyre::eyre!(
                    "expected enclave Quote for registration, got: {other:?}"
                ))
            }
        };

    let input = Bytes::from(
        registerEnclaveCall {
            recipientX25519: U256::from_be_bytes(recipient_x25519),
            attestationPub: U256::from_be_bytes(attestation_pub),
            noiseStaticPub: U256::from_be_bytes(noise_static_pub),
            mrenclave: U256::from_be_bytes(mrenclave.0),
            mrsigner: U256::from_be_bytes(mrsigner.0),
            isvSvn: isv_svn,
        }
        .abi_encode(),
    );

    let signer_address = signer.address();
    let nonce = provider
        .latest()
        .map_err(|e| eyre::eyre!("failed to read latest state for registration nonce: {e}"))?
        .basic_account(&signer_address)
        .map_err(|e| eyre::eyre!("failed to read registration signer account: {e}"))?
        .map(|account| account.nonce)
        .unwrap_or(0);

    let tx = alloy_consensus::TxLegacy {
        chain_id: Some(chain_id),
        nonce,
        gas_price: register_enclave_gas_price(base_fee),
        gas_limit: REGISTER_ENCLAVE_GAS_LIMIT,
        to: TxKind::Call(outbe_primitives::addresses::TEE_REGISTRY_ADDRESS),
        value: U256::ZERO,
        input,
    };
    let signed = signer
        .sign_unsigned(tx)
        .map_err(|e| eyre::eyre!("failed to sign registerEnclave tx: {e}"))?;
    let recovered = signed
        .try_into_recovered()
        .map_err(|_| eyre::eyre!("failed to recover registerEnclave tx signer"))?;
    let pooled = reth_transaction_pool::EthPooledTransaction::new(recovered, 0);

    let outcome = pool
        .add_transaction(reth_transaction_pool::TransactionOrigin::Local, pooled)
        .await
        .map_err(|e| eyre::eyre!("registerEnclave txpool submission failed: {e}"))?;
    Ok(outcome.hash)
}

#[cfg(test)]
mod tests {
    use super::quote_policy_from_tee_policy;
    use alloy_primitives::B256;
    use outbe_primitives::tee_bootstrap::TeePolicy;

    /// Fee handling: the registration tx `gas_price` must be NON-ZERO and clear
    /// both the pool's 7-wei minimum-fee floor and the current block base fee — a
    /// `gas_price = 0` legacy tx is rejected at pool insert and never lands.
    #[test]
    fn register_enclave_gas_price_clears_floor_and_base_fee() {
        use super::{register_enclave_gas_price, REGISTER_ENCLAVE_MIN_BASE_FEE};
        // Never zero (the exact bug being fixed).
        assert!(register_enclave_gas_price(0) > 0);
        // Below the pool floor (base_fee 0/1) → clamped to the floor, then doubled,
        // so the result still clears the floor.
        assert!(register_enclave_gas_price(0) >= REGISTER_ENCLAVE_MIN_BASE_FEE);
        assert!(register_enclave_gas_price(1) >= REGISTER_ENCLAVE_MIN_BASE_FEE);
        // At/above the floor → ≥ the current base fee, with 2× headroom.
        assert_eq!(register_enclave_gas_price(100), 200);
        assert!(register_enclave_gas_price(1_000_000_000) >= 1_000_000_000);
    }

    /// The calldata the node submits MUST decode to the exact signature the
    /// `outbe-teeregistry` precompile dispatches on. A wrong selector or arg order is a
    /// silent on-chain failure (the tx would revert with `unknown selector`), so pin
    /// both: the engine's `registerEnclaveCall` selector equals the keccak of the
    /// canonical signature string (kept byte-identical to the precompile's
    /// `#[contract_public("registerEnclave(...)")]`), and the six args round-trip in
    /// order.
    #[test]
    fn register_enclave_calldata_matches_precompile_signature() {
        use alloy_primitives::{keccak256, U256};
        use alloy_sol_types::SolCall as _;

        // Must stay byte-identical to teeregistry's precompile signature.
        const SIG: &str = "registerEnclave(uint256,uint256,uint256,uint256,uint256,uint16)";
        let expected = &keccak256(SIG.as_bytes())[..4];
        assert_eq!(
            super::registerEnclaveCall::SELECTOR.as_slice(),
            expected,
            "engine calldata selector must match the precompile signature"
        );

        let call = super::registerEnclaveCall {
            recipientX25519: U256::from(0x11),
            attestationPub: U256::from(0x22),
            noiseStaticPub: U256::from(0x33),
            mrenclave: U256::from(0x44),
            mrsigner: U256::from(0x55),
            isvSvn: 7,
        };
        let encoded = call.abi_encode();
        assert_eq!(
            &encoded[..4],
            super::registerEnclaveCall::SELECTOR.as_slice()
        );

        let decoded = super::registerEnclaveCall::abi_decode(&encoded)
            .expect("registerEnclave calldata must decode");
        assert_eq!(decoded.recipientX25519, U256::from(0x11));
        assert_eq!(decoded.attestationPub, U256::from(0x22));
        assert_eq!(decoded.noiseStaticPub, U256::from(0x33));
        assert_eq!(decoded.mrenclave, U256::from(0x44));
        assert_eq!(decoded.mrsigner, U256::from(0x55));
        assert_eq!(decoded.isvSvn, 7);
    }

    /// An empty genesis policy yields the dev-accept connect policy (so an
    /// unattested gramine-direct enclave still connects); a configured policy
    /// yields a strict policy (real quotes are measurement- + DCAP-verified, with
    /// the unattested fallback for the dev box) carrying the allowlist forward.
    #[test]
    fn quote_policy_strict_when_genesis_policy_configured() {
        // Empty -> dev accept any measurement.
        let dev = quote_policy_from_tee_policy(&TeePolicy::default());
        assert!(
            dev.dev_accept_any_measurement,
            "empty policy must be dev-accept"
        );

        // Configured -> strict: no blanket measurement accept, allowlist carried,
        // and only the unattested-fallback escape remains (mode gate, not a relax
        // of real quotes).
        let configured = TeePolicy {
            allowed_mrsigner: vec![B256::repeat_byte(0xAA)],
            allowed_mrenclave: vec![B256::repeat_byte(0xBB)],
            min_isv_svn: 3,
        };
        let strict = quote_policy_from_tee_policy(&configured);
        assert!(
            !strict.dev_accept_any_measurement,
            "configured policy must NOT accept any measurement"
        );
        assert!(
            strict.dev_fallback_if_unattested,
            "mode gate keeps the unattested fallback"
        );
        assert_eq!(strict.allowed_mrsigner, configured.allowed_mrsigner);
        assert_eq!(strict.allowed_mrenclave, configured.allowed_mrenclave);
        assert_eq!(strict.min_isv_svn, 3);
    }
}
