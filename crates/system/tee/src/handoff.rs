//! Tribute offer key-handoff host protocol (Secret-Network-style onboarding).
//!
//! The tribute offer key is resident per-enclave, so a new/returning committee
//! member does not run a DKG reshare — it asks the committee to hand off the key.
//! A newcomer broadcasts its attested quote; any current node with the resident
//! key verifies the quote, seals the group signature to the newcomer's attested
//! X25519 key, and replies. The newcomer's enclave ingests the first reply that
//! verifies against the on-chain offer public (1-of-n — a malicious server cannot
//! install a wrong key, see [`crate::verify_peer_quote`] + the enclave's
//! `IngestTributeOfferHandoff` seam).
//!
//! The drivers are generic over [`EnclaveChannel`] (the enclave) and
//! [`BootstrapGossip`] (the P2P carrier) so they are unit-testable with an
//! in-memory gossip + a mock enclave, and run in the node with [`crate::EnclaveClient`]
//! over the muxed consensus channel.

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

use crate::client::{verify_peer_quote, AttestedPeerKeys, QuotePolicy};
use crate::protocol::{EnclaveRequest, EnclaveResponse};
use crate::tee_dkg::{CeremonyError, EnclaveChannel};

/// One poll of the handoff carrier: a peer message, an idle tick (no message within
/// the carrier's poll window — the newcomer re-broadcasts its request), or channel
/// closure. The idle tick lets a one-shot-lost request be re-asked instead of
/// burning the whole deadline (mirrors the DKG identity-exchange re-announce).
#[derive(Debug)]
pub enum HandoffEvent {
    /// A frame from `peer`.
    Message { peer: Vec<u8>, bytes: Vec<u8> },
    /// No frame within the poll window; safe to re-broadcast and keep waiting.
    Idle,
    /// The carrier terminated.
    Closed,
}

/// P2P carrier for the handoff sub-protocol. Like `BootstrapGossip` but `recv`
/// yields the sender's id so the newcomer can require confirmations from a QUORUM
/// of DISTINCT responders (anti-equivocation / availability), not just one, and
/// surfaces an idle tick so the newcomer can re-broadcast a lost request.
#[allow(async_fn_in_trait)]
pub trait HandoffGossip {
    /// Broadcast `bytes` to the committee.
    async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError>;
    /// Poll for the next event: a peer message, an idle tick, or closure. An
    /// implementation SHOULD bound the wait so [`HandoffEvent::Idle`] is produced
    /// periodically while no message arrives.
    async fn recv(&mut self) -> HandoffEvent;
}

/// Wire messages for the key-handoff sub-protocol (newcomer ⇄ current node).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandoffWireMessage {
    /// Newcomer → committee: "I need the offer key; here is my attested quote."
    /// The server re-verifies `quote` with [`verify_peer_quote`] and seals to the
    /// attested `recipient_x25519` it carries. Boxed: the `Quote` response is large
    /// relative to a `Reply`.
    Request { quote: Box<EnclaveResponse> },
    /// Current node → newcomer: the resident group signature sealed to the
    /// requester's attested X25519 key (an opaque `EncryptedShare` blob).
    Reply { sealed: Vec<u8> },
}

impl HandoffWireMessage {
    /// Postcard-encode for the wire.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CeremonyError> {
        postcard::to_allocvec(self).map_err(|_| CeremonyError::MalformedWire("encode handoff"))
    }
    /// Decode a wire message; `None` on a malformed frame.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// NEWCOMER side: obtain the resident offer key from the committee via key-handoff.
///
/// Broadcasts this enclave's attested `quote`, then ingests replies that verify
/// against `expected_tribute_offer_public` (the on-chain registered key). The
/// enclave installs the key write-once on the first valid reply; this returns `Ok`
/// once `min_confirmations` DISTINCT responders have each provided a verifying reply
/// (quorum — anti-equivocation + availability; correctness is already
/// guaranteed by the on-chain check, so `min_confirmations = 1` is the safe 1-of-n
/// floor). Invalid/duplicate-responder replies are ignored. The caller bounds the
/// wait (deadline/timeout) and obtains `my_quote` from [`crate::EnclaveClient::quote`].
pub async fn run_handoff_as_newcomer<C, G>(
    enclave: &mut C,
    gossip: &mut G,
    my_quote: EnclaveResponse,
    expected_tribute_offer_public: [u8; 32],
    chain_id: B256,
    tribute_offer_epoch: u64,
    min_confirmations: usize,
) -> Result<(), CeremonyError>
where
    C: EnclaveChannel,
    G: HandoffGossip,
{
    let request = HandoffWireMessage::Request {
        quote: Box::new(my_quote),
    };
    let request_bytes = request.to_bytes()?;
    gossip.broadcast(request_bytes.clone()).await?;

    let need = min_confirmations.max(1);
    let mut confirmed: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    loop {
        let (peer, raw) = match gossip.recv().await {
            HandoffEvent::Message { peer, bytes } => (peer, bytes),
            // No reply yet — re-broadcast the request (a one-shot broadcast can be
            // lost to a startup P2P-session race) and keep waiting. The caller's
            // deadline bounds the total wait.
            HandoffEvent::Idle => {
                gossip.broadcast(request_bytes.clone()).await?;
                continue;
            }
            HandoffEvent::Closed => {
                return Err(CeremonyError::UnexpectedResponse(
                    "handoff gossip closed before the offer key reached quorum",
                ));
            }
        };
        let Some(HandoffWireMessage::Reply { sealed }) = HandoffWireMessage::from_bytes(&raw)
        else {
            continue; // ignore requests / malformed frames
        };
        if confirmed.contains(&peer) {
            continue; // one confirmation per responder
        }
        // A single bad reply must NOT abort onboarding. The enclave returns an
        // `Error` response (mapped to `Err` by the production client) when the sealed
        // blob does not derive the on-chain key; treat that — and any per-reply
        // transport error — as "this reply is no good, wait for another responder",
        // not a fatal `?` that ends the whole handoff.
        let ingested = enclave.request(&EnclaveRequest::IngestTributeOfferHandoff {
            sealed,
            expected_tribute_offer_public,
            chain_id,
            tribute_offer_epoch,
        });
        match ingested {
            Ok(EnclaveResponse::TributeOfferHandoffIngested { .. }) => {
                // Idempotent after the first install (same on-chain-verified key);
                // count distinct responders toward quorum.
                confirmed.insert(peer);
                if confirmed.len() >= need {
                    return Ok(());
                }
            }
            // A reply whose sealed key does not match the on-chain expected public is
            // rejected by the enclave (Error response or a transport error); skip it
            // and wait for another responder.
            _ => continue,
        }
    }
}

/// SERVER side: answer one handoff request. Verifies the requester's quote under
/// `verify_policy`, lets the caller authorize it (e.g. the P2P sender's consensus
/// key is in the active committee), then asks this node's enclave to seal the
/// resident group signature to the attested X25519 key. Returns the reply bytes to
/// gossip back, or `None` to ignore the request (bad quote, not authorized, no
/// resident key, or a malformed frame).
pub fn answer_handoff_request<C: EnclaveChannel>(
    enclave: &mut C,
    request_bytes: &[u8],
    verify_policy: &QuotePolicy,
    authorize: impl FnOnce(&AttestedPeerKeys) -> bool,
) -> Option<Vec<u8>> {
    let HandoffWireMessage::Request { quote } = HandoffWireMessage::from_bytes(request_bytes)?
    else {
        return None;
    };
    let attested = verify_peer_quote(&quote, verify_policy).ok()?;
    if !authorize(&attested) {
        return None;
    }
    match enclave
        .request(&EnclaveRequest::SealTributeOfferHandoff {
            recipient_x25519: attested.recipient_x25519,
        })
        .ok()?
    {
        EnclaveResponse::SealedTributeOfferHandoff { sealed } => {
            HandoffWireMessage::Reply { sealed }.to_bytes().ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;
    use std::collections::VecDeque;

    /// Build an attested-looking `Quote` with a valid REPORT_DATA key binding and an
    /// empty (unattested) `quote_body` — accepted by a dev policy.
    fn dev_quote(seed: u8) -> EnclaveResponse {
        let recipient_x25519 = [seed; 32];
        let attestation_pub = [seed.wrapping_add(1); 32];
        let noise_static_pub = [seed.wrapping_add(2); 32];
        let mut preimage = Vec::with_capacity(96);
        preimage.extend_from_slice(&noise_static_pub);
        preimage.extend_from_slice(&recipient_x25519);
        preimage.extend_from_slice(&attestation_pub);
        EnclaveResponse::Quote {
            mrenclave: B256::ZERO,
            mrsigner: B256::ZERO,
            isv_svn: 0,
            report_data: keccak256(&preimage),
            recipient_x25519_pub: recipient_x25519,
            attestation_pub,
            noise_static_pub,
            quote_body: Vec::new(),
            attestation: "none (test)".to_string(),
        }
    }

    /// A mock enclave: the server seals to whatever recipient is asked; the newcomer
    /// ingests successfully only when the sealed blob equals `good_sealed` AND the
    /// expected public equals `target_public`.
    struct MockEnclave {
        sealed: Vec<u8>,
        good_sealed: Vec<u8>,
        target_public: [u8; 32],
    }
    impl EnclaveChannel for MockEnclave {
        fn request(
            &mut self,
            req: &EnclaveRequest,
        ) -> Result<EnclaveResponse, crate::errors::TransportError> {
            Ok(match req {
                EnclaveRequest::SealTributeOfferHandoff { .. } => {
                    EnclaveResponse::SealedTributeOfferHandoff {
                        sealed: self.sealed.clone(),
                    }
                }
                EnclaveRequest::IngestTributeOfferHandoff {
                    sealed,
                    expected_tribute_offer_public,
                    ..
                } => {
                    if *sealed == self.good_sealed
                        && *expected_tribute_offer_public == self.target_public
                    {
                        EnclaveResponse::TributeOfferHandoffIngested {
                            tribute_offer_public: self.target_public,
                        }
                    } else {
                        EnclaveResponse::Error {
                            message: "rejected".to_string(),
                        }
                    }
                }
                _ => EnclaveResponse::Error {
                    message: "unexpected".to_string(),
                },
            })
        }
    }

    /// In-memory gossip: `recv` drains a pre-loaded `(peer, bytes)` queue as
    /// `Message`s, then signals `Closed`; `broadcast` records sends.
    struct VecGossip {
        incoming: VecDeque<(Vec<u8>, Vec<u8>)>,
        outgoing: Vec<Vec<u8>>,
    }
    impl HandoffGossip for VecGossip {
        async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError> {
            self.outgoing.push(bytes);
            Ok(())
        }
        async fn recv(&mut self) -> HandoffEvent {
            match self.incoming.pop_front() {
                Some((peer, bytes)) => HandoffEvent::Message { peer, bytes },
                None => HandoffEvent::Closed,
            }
        }
    }

    /// Gossip that returns `Idle` for the first `idle_ticks` polls (forcing the
    /// newcomer to re-broadcast), then drains its queued `Message`s. Used to prove
    /// a lost first request is re-sent.
    struct IdleThenVecGossip {
        idle_ticks: usize,
        incoming: VecDeque<(Vec<u8>, Vec<u8>)>,
        broadcasts: usize,
    }
    impl HandoffGossip for IdleThenVecGossip {
        async fn broadcast(&mut self, _bytes: Vec<u8>) -> Result<(), CeremonyError> {
            self.broadcasts += 1;
            Ok(())
        }
        async fn recv(&mut self) -> HandoffEvent {
            if self.idle_ticks > 0 {
                self.idle_ticks -= 1;
                return HandoffEvent::Idle;
            }
            match self.incoming.pop_front() {
                Some((peer, bytes)) => HandoffEvent::Message { peer, bytes },
                None => HandoffEvent::Closed,
            }
        }
    }

    #[test]
    fn handoff_wire_message_roundtrips() {
        for msg in [
            HandoffWireMessage::Request {
                quote: Box::new(dev_quote(7)),
            },
            HandoffWireMessage::Reply {
                sealed: vec![1, 2, 3, 4],
            },
        ] {
            let bytes = msg.to_bytes().unwrap();
            assert_eq!(HandoffWireMessage::from_bytes(&bytes), Some(msg));
        }
    }

    #[test]
    fn server_verifies_quote_and_seals() {
        let mut server = MockEnclave {
            sealed: vec![0xAB; 12],
            good_sealed: vec![],
            target_public: [0; 32],
        };
        let request = HandoffWireMessage::Request {
            quote: Box::new(dev_quote(9)),
        }
        .to_bytes()
        .unwrap();
        let reply = answer_handoff_request(
            &mut server,
            &request,
            &QuotePolicy::dev_accept_any(),
            |_| true,
        )
        .expect("server replies");
        assert_eq!(
            HandoffWireMessage::from_bytes(&reply),
            Some(HandoffWireMessage::Reply {
                sealed: vec![0xAB; 12]
            })
        );
    }

    #[test]
    fn server_binds_seal_target_to_registered_recipient() {
        // The responder's authorize closure binds the requester's quote
        // `recipient_x25519` to the on-chain registered key. `dev_quote(seed)` carries
        // `recipient_x25519 = [seed; 32]`, so the registered key for `seed = 9` is
        // `[9; 32]`. The closure mirrors `serve_tee_handoff`'s binding.
        let registered: [u8; 32] = [9; 32];
        let request = HandoffWireMessage::Request {
            quote: Box::new(dev_quote(9)),
        }
        .to_bytes()
        .unwrap();

        // Matching registration → served.
        let mut server = MockEnclave {
            sealed: vec![0xAB; 12],
            good_sealed: vec![],
            target_public: [0; 32],
        };
        let reply = answer_handoff_request(
            &mut server,
            &request,
            &QuotePolicy::dev_accept_any(),
            |attested| attested.recipient_x25519 == registered,
        );
        assert!(reply.is_some(), "matching recipient must be served");

        // Mismatched registration (the requester presents a recipient it did not
        // register) → refused, even though the quote verifies and the peer is in the
        // active set.
        let wrong_registered: [u8; 32] = [0xAA; 32];
        let mut server2 = MockEnclave {
            sealed: vec![0xAB; 12],
            good_sealed: vec![],
            target_public: [0; 32],
        };
        let reply2 = answer_handoff_request(
            &mut server2,
            &request,
            &QuotePolicy::dev_accept_any(),
            |attested| attested.recipient_x25519 == wrong_registered,
        );
        assert!(reply2.is_none(), "mismatched recipient must be refused");
    }

    #[test]
    fn server_ignores_unauthorized_or_bad_quote() {
        let mut server = MockEnclave {
            sealed: vec![0xAB; 12],
            good_sealed: vec![],
            target_public: [0; 32],
        };
        let request = HandoffWireMessage::Request {
            quote: Box::new(dev_quote(9)),
        }
        .to_bytes()
        .unwrap();
        // Not authorized → no reply.
        assert!(answer_handoff_request(
            &mut server,
            &request,
            &QuotePolicy::dev_accept_any(),
            |_| false
        )
        .is_none());

        // Tampered report_data binding → quote verification fails → no reply.
        let mut bad = dev_quote(9);
        if let EnclaveResponse::Quote { report_data, .. } = &mut bad {
            *report_data = B256::repeat_byte(0xFF);
        }
        let bad_request = HandoffWireMessage::Request {
            quote: Box::new(bad),
        }
        .to_bytes()
        .unwrap();
        assert!(answer_handoff_request(
            &mut server,
            &bad_request,
            &QuotePolicy::dev_accept_any(),
            |_| true
        )
        .is_none());
    }

    #[tokio::test]
    async fn newcomer_ingests_first_valid_reply() {
        let target = [0x5a; 32];
        let good_sealed = vec![0xAB; 12];
        let mut newcomer = MockEnclave {
            sealed: vec![],
            good_sealed: good_sealed.clone(),
            target_public: target,
        };
        // A bad reply (wrong sealed) precedes the good one; the newcomer must ignore
        // the bad reply and accept the good one (1-of-n: min_confirmations = 1).
        let mut gossip = VecGossip {
            incoming: VecDeque::from(vec![
                (
                    b"peerA".to_vec(),
                    HandoffWireMessage::Reply {
                        sealed: vec![0xFF; 5],
                    }
                    .to_bytes()
                    .unwrap(),
                ),
                (
                    b"peerB".to_vec(),
                    HandoffWireMessage::Reply {
                        sealed: good_sealed,
                    }
                    .to_bytes()
                    .unwrap(),
                ),
            ]),
            outgoing: Vec::new(),
        };
        run_handoff_as_newcomer(
            &mut newcomer,
            &mut gossip,
            dev_quote(3),
            target,
            B256::ZERO,
            0,
            1,
        )
        .await
        .expect("newcomer obtains the key from the first valid reply");
        // It broadcast exactly one request.
        assert_eq!(gossip.outgoing.len(), 1);
    }

    #[tokio::test]
    async fn newcomer_rebroadcasts_request_on_idle() {
        // The first request is "lost" (2 idle ticks) before any reply arrives; the
        // newcomer must re-broadcast on each idle tick rather than wait out the
        // deadline, then ingest the reply that follows.
        let target = [0x5a; 32];
        let good_sealed = vec![0xAB; 12];
        let mut newcomer = MockEnclave {
            sealed: vec![],
            good_sealed: good_sealed.clone(),
            target_public: target,
        };
        let mut gossip = IdleThenVecGossip {
            idle_ticks: 2,
            incoming: VecDeque::from(vec![(
                b"peerA".to_vec(),
                HandoffWireMessage::Reply {
                    sealed: good_sealed,
                }
                .to_bytes()
                .unwrap(),
            )]),
            broadcasts: 0,
        };
        run_handoff_as_newcomer(
            &mut newcomer,
            &mut gossip,
            dev_quote(3),
            target,
            B256::ZERO,
            0,
            1,
        )
        .await
        .expect("newcomer obtains the key after re-broadcasting");
        // 1 initial broadcast + 2 re-broadcasts (one per idle tick).
        assert_eq!(gossip.broadcasts, 3, "must re-broadcast on each idle tick");
    }

    /// A per-reply enclave transport error (the production client maps an enclave
    /// `Error` response to `Err`) must NOT abort the whole handoff — the newcomer skips
    /// that reply and ingests the next responder's.
    #[tokio::test]
    async fn newcomer_survives_per_reply_transport_error() {
        /// Enclave that errors on the FIRST ingest, then succeeds.
        struct ErrThenOkEnclave {
            fail_first: bool,
            target: [u8; 32],
        }
        impl EnclaveChannel for ErrThenOkEnclave {
            fn request(
                &mut self,
                req: &EnclaveRequest,
            ) -> Result<EnclaveResponse, crate::errors::TransportError> {
                match req {
                    EnclaveRequest::IngestTributeOfferHandoff { .. } => {
                        if self.fail_first {
                            self.fail_first = false;
                            Err(crate::errors::TransportError::EnclaveError(
                                "rejected reply".to_string(),
                            ))
                        } else {
                            Ok(EnclaveResponse::TributeOfferHandoffIngested {
                                tribute_offer_public: self.target,
                            })
                        }
                    }
                    _ => Ok(EnclaveResponse::Error {
                        message: "unexpected".to_string(),
                    }),
                }
            }
        }

        let target = [0x5a; 32];
        let mut newcomer = ErrThenOkEnclave {
            fail_first: true,
            target,
        };
        let reply: Vec<u8> = HandoffWireMessage::Reply {
            sealed: vec![0xAB; 12],
        }
        .to_bytes()
        .unwrap();
        let mut gossip = VecGossip {
            incoming: VecDeque::from(vec![
                (b"peerA".to_vec(), reply.clone()), // first ingest → Err, skipped
                (b"peerB".to_vec(), reply),         // second ingest → Ok
            ]),
            outgoing: Vec::new(),
        };
        run_handoff_as_newcomer(
            &mut newcomer,
            &mut gossip,
            dev_quote(3),
            target,
            B256::ZERO,
            0,
            1,
        )
        .await
        .expect("a per-reply error must not abort onboarding");
    }

    #[tokio::test]
    async fn newcomer_waits_for_quorum_distinct_responders() {
        let target = [0x5a; 32];
        let good_sealed = vec![0xAB; 12];
        let mut newcomer = MockEnclave {
            sealed: vec![],
            good_sealed: good_sealed.clone(),
            target_public: target,
        };
        let good: Vec<u8> = HandoffWireMessage::Reply {
            sealed: good_sealed.clone(),
        }
        .to_bytes()
        .unwrap();
        // Two replies from the SAME responder + one from a second: a 2-of-n quorum
        // must NOT be satisfied by the duplicate — only distinct responders count.
        let mut gossip = VecGossip {
            incoming: VecDeque::from(vec![
                (b"peerA".to_vec(), good.clone()),
                (b"peerA".to_vec(), good.clone()), // duplicate — ignored
                (b"peerB".to_vec(), good.clone()),
            ]),
            outgoing: Vec::new(),
        };
        run_handoff_as_newcomer(
            &mut newcomer,
            &mut gossip,
            dev_quote(3),
            target,
            B256::ZERO,
            0,
            2, // quorum = 2 distinct responders
        )
        .await
        .expect("two distinct responders confirm the key");
    }
}
