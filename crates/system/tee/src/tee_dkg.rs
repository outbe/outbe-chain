//! Host-side TEE DKG ceremony coordinator.
//!
//! This is the TEE-native equivalent of the consensus DKG actor
//! (`crates/blockchain/consensus/src/dkg_actor`). The public protocol (P2P
//! gossip, ceremony bookkeeping, message shaping) runs on the host. The host
//! sends every secret-touching operation to the enclave over the Noise-IK
//! channel (the [`EnclaveChannel`] / `EnclaveClient` protocol). Unlike a literal
//! clone of the consensus actor, no `Dealer`/`Player` ever runs on the host, so
//! shares and the assembled key never appear in host memory.
//!
//! The coordinator exposes the ceremony as explicit phase methods. Each method
//! (a) calls the enclave seam and (b) shapes the resulting host wire messages. A
//! driver (the production commonware-P2P event loop, or the in-process e2e test
//! harness) routes [`DealerBundle`] / [`Ack`] / [`FinalizedLog`] messages between
//! peers. The driver feeds them back into the matching phase method. The wire
//! messages carry only opaque bytes (the host never decodes Commonware types -
//! the enclave does).
//!
//! Production note: the threshold/timeout/retry event loop that drives these
//! phases over real commonware P2P gossip is the remaining host-integration piece
//! (validated on the localnet). This module is the seam-routing + message-shaping
//! core that loop builds on. `bin/outbe-tee-enclave/tests/dkg_e2e.rs` validates
//! this module end-to-end over the real Noise-IK transport.

use alloy_primitives::B256;

use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};

mod ceremony;

pub use ceremony::{run_tee_dkg_ceremony, FoundingCeremonyParameters};

/// The host's channel to its enclave: a request/response transport.
/// [`crate::EnclaveClient`] implements it over Noise-IK. The trait hides the
/// transport, so the coordinator is testable and transport-agnostic.
pub trait EnclaveChannel {
    fn request(
        &mut self,
        req: &EnclaveRequest,
    ) -> core::result::Result<EnclaveResponse, TransportError>;
}

impl EnclaveChannel for crate::EnclaveClient {
    fn request(
        &mut self,
        req: &EnclaveRequest,
    ) -> core::result::Result<EnclaveResponse, TransportError> {
        crate::EnclaveClient::request(self, req)
    }
}

impl EnclaveChannel for crate::AuthorizedEnclaveClient {
    fn request(
        &mut self,
        req: &EnclaveRequest,
    ) -> core::result::Result<EnclaveResponse, TransportError> {
        crate::AuthorizedEnclaveClient::request(self, req)
    }
}

impl EnclaveChannel for crate::RuntimeEnclaveClient {
    fn request(
        &mut self,
        req: &EnclaveRequest,
    ) -> core::result::Result<EnclaveResponse, TransportError> {
        crate::RuntimeEnclaveClient::request(self, req)
    }
}

/// A dealer's sealed dealing to one recipient (dealer -> player).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DealerBundle {
    pub dealer_bls: Vec<u8>,
    pub pub_msg: Vec<u8>,
    pub sealed_share: Vec<u8>,
}

/// A player's acknowledgement of a verified dealing (player -> dealer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    pub player_bls: Vec<u8>,
    pub ack: Vec<u8>,
}

/// A dealer's signed log of its completed dealing (dealer -> all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedLog {
    pub dealer_bls: Vec<u8>,
    pub signed_log: Vec<u8>,
}

/// A TEE DKG gossip message, carried over the consensus P2P layer. All fields are
/// opaque bytes (the host never decodes Commonware types - the enclave does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DkgWireMessage {
    DealerBundle(DealerBundle),
    Ack(Ack),
    FinalizedLog(FinalizedLog),
    /// Seam F: a participant's partial signature over the fixed offer message,
    /// **sealed to one recipient enclave** (`partial` is opaque ciphertext). Each
    /// signer broadcasts one of these per recipient. A recipient collects the
    /// ciphertexts addressed to it (`recipient_bls == its enclave`) and recovers
    /// the offer key in-SGX. The host cannot decrypt them.
    TributeOfferPartial {
        signer_bls: Vec<u8>,
        recipient_bls: Vec<u8>,
        partial: Vec<u8>,
    },
}

impl DkgWireMessage {
    /// Encode to the deterministic wire format `tag(1) || [u32 len || bytes]...`
    /// so the consensus P2P adapter can ship it as an opaque payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        fn put(buf: &mut Vec<u8>, field: &[u8]) {
            buf.extend_from_slice(&(field.len() as u32).to_be_bytes());
            buf.extend_from_slice(field);
        }
        let mut buf = Vec::new();
        match self {
            DkgWireMessage::DealerBundle(b) => {
                buf.push(0);
                put(&mut buf, &b.dealer_bls);
                put(&mut buf, &b.pub_msg);
                put(&mut buf, &b.sealed_share);
            }
            DkgWireMessage::Ack(a) => {
                buf.push(1);
                put(&mut buf, &a.player_bls);
                put(&mut buf, &a.ack);
            }
            DkgWireMessage::FinalizedLog(l) => {
                buf.push(2);
                put(&mut buf, &l.dealer_bls);
                put(&mut buf, &l.signed_log);
            }
            DkgWireMessage::TributeOfferPartial {
                signer_bls,
                recipient_bls,
                partial,
            } => {
                buf.push(3);
                put(&mut buf, signer_bls);
                put(&mut buf, recipient_bls);
                put(&mut buf, partial);
            }
        }
        buf
    }

    /// Decode the wire format produced by [`DkgWireMessage::to_bytes`]. Rejects a
    /// malformed or trailing-byte payload.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut offset = 0usize;
        let tag = *bytes
            .first()
            .ok_or(CeremonyError::MalformedWire("empty payload"))?;
        offset += 1;
        let take = |offset: &mut usize| -> Result<Vec<u8>> {
            let len_end = offset
                .checked_add(4)
                .filter(|end| *end <= bytes.len())
                .ok_or(CeremonyError::MalformedWire("truncated length"))?;
            let len = u32::from_be_bytes([
                bytes[*offset],
                bytes[*offset + 1],
                bytes[*offset + 2],
                bytes[*offset + 3],
            ]) as usize;
            let end = len_end
                .checked_add(len)
                .filter(|end| *end <= bytes.len())
                .ok_or(CeremonyError::MalformedWire("truncated field"))?;
            let field = bytes[len_end..end].to_vec();
            *offset = end;
            Ok(field)
        };
        let msg = match tag {
            0 => DkgWireMessage::DealerBundle(DealerBundle {
                dealer_bls: take(&mut offset)?,
                pub_msg: take(&mut offset)?,
                sealed_share: take(&mut offset)?,
            }),
            1 => DkgWireMessage::Ack(Ack {
                player_bls: take(&mut offset)?,
                ack: take(&mut offset)?,
            }),
            2 => DkgWireMessage::FinalizedLog(FinalizedLog {
                dealer_bls: take(&mut offset)?,
                signed_log: take(&mut offset)?,
            }),
            3 => DkgWireMessage::TributeOfferPartial {
                signer_bls: take(&mut offset)?,
                recipient_bls: take(&mut offset)?,
                partial: take(&mut offset)?,
            },
            _ => return Err(CeremonyError::MalformedWire("unknown tag")),
        };
        if offset != bytes.len() {
            return Err(CeremonyError::MalformedWire("trailing bytes"));
        }
        Ok(msg)
    }
}

/// The P2P gossip surface the ceremony driver needs. The node implements it over
/// the consensus P2P channel. An in-memory implementation drives the end-to-end
/// test. Async because real P2P send/recv is async.
#[allow(async_fn_in_trait)]
pub trait DkgGossip {
    /// Send a message to one peer, addressed by BLS public key bytes.
    async fn send(&mut self, to: &[u8], msg: DkgWireMessage) -> Result<()>;
    /// Broadcast a message to every peer.
    async fn broadcast(&mut self, msg: DkgWireMessage) -> Result<()>;
    /// Receive the next `(from_bls, msg)`, or `None` once the ceremony's inputs
    /// are exhausted / the channel closes.
    async fn recv(&mut self) -> Option<(Vec<u8>, DkgWireMessage)>;
}

/// A bundle addressed to a specific recipient by BLS pubkey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressed<T> {
    pub to: Vec<u8>,
    pub msg: T,
}

/// The completed ceremony result for this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeremonyOutcome {
    /// Public group threshold key (encoded). Identical for all honest parties.
    pub group_public: Vec<u8>,
    /// Commitment to this node's secret threshold share (the share stays in SGX).
    pub share_commitment: B256,
    /// The shared tribute offer X25519 public key, derived from the group
    /// threshold signature over the fixed offer message (Seam F). Byte-identical
    /// for all honest parties. Clients encrypt offers to it. Set by
    /// [`run_tee_dkg_ceremony`]. It stays `[0u8; 32]` until Seam F completes.
    pub tribute_offer_public: [u8; 32],
    /// The committee's encoded DKG group public key (constant term). Set alongside
    /// `tribute_offer_public` at Seam F and carried into the founding bootstrap
    /// payload.
    pub tribute_offer_group_public_key: Vec<u8>,
}

/// Coordinator errors: a transport failure, or an unexpected enclave response.
#[derive(Debug, thiserror::Error)]
pub enum CeremonyError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("unexpected enclave response for {0}")]
    UnexpectedResponse(&'static str),
    #[error("malformed DKG wire message: {0}")]
    MalformedWire(&'static str),
    #[error("enclave error: {0}")]
    EnclaveError(String),
    #[error("bounded delivery error: {0}")]
    Delivery(String),
}

type Result<T> = core::result::Result<T, CeremonyError>;

/// Drives one node's participation in a TEE DKG ceremony by routing each phase to
/// the enclave and shaping the host wire messages.
pub struct CeremonyCoordinator {
    ceremony_id: B256,
    round: u64,
    my_bls: Vec<u8>,
    participants: Vec<crate::protocol::ParticipantAnnounce>,
}

impl CeremonyCoordinator {
    /// `participants` is each enclave's `ParticipantAnnounce`.
    /// Each value comes from `DkgParticipantAnnounceV1`, not from `GetPublicKeys`.
    /// The binding signature is the enclave TEE-BLS signature.
    pub fn new(
        ceremony_id: B256,
        round: u64,
        my_bls: Vec<u8>,
        participants: Vec<crate::protocol::ParticipantAnnounce>,
    ) -> Self {
        Self {
            ceremony_id,
            round,
            my_bls,
            participants,
        }
    }

    /// This node's BLS public key bytes.
    pub fn my_bls(&self) -> &[u8] {
        &self.my_bls
    }

    /// Open the ceremony inside the enclave.
    pub fn open<C: EnclaveChannel>(&self, ch: &mut C) -> Result<()> {
        match ch.request(&EnclaveRequest::DkgOpen {
            ceremony_id: self.ceremony_id,
            round: self.round,
            participants: self.participants.clone(),
        })? {
            EnclaveResponse::Ack => Ok(()),
            _ => Err(CeremonyError::UnexpectedResponse("DkgOpen")),
        }
    }

    /// Seam A: deal and produce one [`DealerBundle`] per recipient.
    pub fn deal<C: EnclaveChannel>(&self, ch: &mut C) -> Result<Vec<Addressed<DealerBundle>>> {
        match ch.request(&EnclaveRequest::DkgStartDealer {
            ceremony_id: self.ceremony_id,
        })? {
            EnclaveResponse::DkgDealt {
                pub_msg,
                sealed_shares,
            } => Ok(sealed_shares
                .into_iter()
                .map(|(recipient_bls, sealed_share)| Addressed {
                    to: recipient_bls,
                    msg: DealerBundle {
                        dealer_bls: self.my_bls.clone(),
                        pub_msg: pub_msg.clone(),
                        sealed_share,
                    },
                })
                .collect()),
            _ => Err(CeremonyError::UnexpectedResponse("DkgStartDealer")),
        }
    }

    /// Seam B: open + verify an incoming dealing; produce an [`Ack`] addressed to
    /// the dealer (or `None` if the dealing did not verify).
    pub fn ingest<C: EnclaveChannel>(
        &self,
        ch: &mut C,
        bundle: &DealerBundle,
    ) -> Result<Option<Addressed<Ack>>> {
        match ch.request(&EnclaveRequest::DkgPlayerIngest {
            ceremony_id: self.ceremony_id,
            dealer_bls: bundle.dealer_bls.clone(),
            pub_msg: bundle.pub_msg.clone(),
            sealed_share: bundle.sealed_share.clone(),
        })? {
            EnclaveResponse::DkgPlayerAck { ack } => Ok(ack.map(|ack| Addressed {
                to: bundle.dealer_bls.clone(),
                msg: Ack {
                    player_bls: self.my_bls.clone(),
                    ack,
                },
            })),
            _ => Err(CeremonyError::UnexpectedResponse("DkgPlayerIngest")),
        }
    }

    /// Seam C: record a player's ack at this node's dealer.
    pub fn receive_ack<C: EnclaveChannel>(&self, ch: &mut C, ack: &Ack) -> Result<()> {
        match ch.request(&EnclaveRequest::DkgDealerReceiveAck {
            ceremony_id: self.ceremony_id,
            player_bls: ack.player_bls.clone(),
            ack: ack.ack.clone(),
        })? {
            EnclaveResponse::Ack => Ok(()),
            _ => Err(CeremonyError::UnexpectedResponse("DkgDealerReceiveAck")),
        }
    }

    /// Seam D: finalize this node's dealing into a broadcastable [`FinalizedLog`].
    pub fn finalize_dealer<C: EnclaveChannel>(&self, ch: &mut C) -> Result<FinalizedLog> {
        match ch.request(&EnclaveRequest::DkgDealerFinalize {
            ceremony_id: self.ceremony_id,
        })? {
            EnclaveResponse::DkgSignedLog { signed_log } => Ok(FinalizedLog {
                dealer_bls: self.my_bls.clone(),
                signed_log,
            }),
            _ => Err(CeremonyError::UnexpectedResponse("DkgDealerFinalize")),
        }
    }

    /// Seam E: verify the collected dealer logs and recover this node's threshold
    /// share inside the enclave. Return the public outcome.
    pub fn finalize_player<C: EnclaveChannel>(
        &self,
        ch: &mut C,
        logs: &[FinalizedLog],
    ) -> Result<CeremonyOutcome> {
        let signed_logs = logs.iter().map(|l| l.signed_log.clone()).collect();
        match ch.request(&EnclaveRequest::DkgPlayerFinalize {
            ceremony_id: self.ceremony_id,
            signed_logs,
        })? {
            EnclaveResponse::DkgPlayerFinalized {
                group_public,
                share_commitment,
            } => Ok(CeremonyOutcome {
                group_public,
                share_commitment,
                // Filled by `run_tee_dkg_ceremony` after Seam F.
                tribute_offer_public: [0u8; 32],
                tribute_offer_group_public_key: Vec::new(),
            }),
            _ => Err(CeremonyError::UnexpectedResponse("DkgPlayerFinalize")),
        }
    }

    /// Seam F: threshold-sign the fixed offer message with this node's share, then
    /// seal the partial to each recipient enclave. Returns one
    /// `(recipient_bls, sealed_partial)` per participant. The caller gossips each
    /// sealed ciphertext to its recipient. The host never sees a plaintext partial.
    pub fn tribute_offer_partials_sealed<C: EnclaveChannel>(
        &self,
        ch: &mut C,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        match ch.request(&EnclaveRequest::DkgTributeOfferPartial {
            ceremony_id: self.ceremony_id,
        })? {
            EnclaveResponse::DkgTributeOfferPartial { sealed } => Ok(sealed),
            _ => Err(CeremonyError::UnexpectedResponse("DkgTributeOfferPartial")),
        }
    }

    /// Founding Seam F: finalize the group threshold signature from the sealed
    /// partials addressed to this enclave and install the permanent offer key.
    /// The enclave capability matrix rejects this request once the key is ready.
    pub fn finalize_tribute_offer<C: EnclaveChannel>(
        &self,
        ch: &mut C,
        sealed_partials: &[Vec<u8>],
        chain_id: B256,
        tribute_offer_epoch: u64,
    ) -> Result<([u8; 32], Vec<u8>)> {
        match ch.request(&EnclaveRequest::DkgFinalizeTributeOffer {
            ceremony_id: self.ceremony_id,
            sealed_partials: sealed_partials.to_vec(),
            chain_id,
            tribute_offer_epoch,
        })? {
            EnclaveResponse::DkgTributeOfferKey {
                tribute_offer_public,
                group_public_key,
            } => Ok((tribute_offer_public, group_public_key)),
            EnclaveResponse::Error { message } => Err(CeremonyError::EnclaveError(message)),
            _ => Err(CeremonyError::UnexpectedResponse("DkgFinalizeTributeOffer")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_messages() -> Vec<DkgWireMessage> {
        vec![
            DkgWireMessage::DealerBundle(DealerBundle {
                dealer_bls: vec![1, 2, 3],
                pub_msg: vec![4; 40],
                sealed_share: vec![5; 80],
            }),
            DkgWireMessage::Ack(Ack {
                player_bls: vec![6, 7],
                ack: vec![8; 50],
            }),
            DkgWireMessage::FinalizedLog(FinalizedLog {
                dealer_bls: vec![9],
                signed_log: vec![10; 120],
            }),
            DkgWireMessage::TributeOfferPartial {
                signer_bls: vec![11, 12, 13],
                recipient_bls: vec![21, 22, 23],
                partial: vec![14; 48],
            },
        ]
    }

    #[test]
    fn dkg_wire_message_roundtrips() {
        for msg in sample_messages() {
            let bytes = msg.to_bytes();
            assert_eq!(DkgWireMessage::from_bytes(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn dkg_wire_message_rejects_malformed() {
        assert!(DkgWireMessage::from_bytes(&[]).is_err());
        assert!(DkgWireMessage::from_bytes(&[9]).is_err()); // unknown tag
        let mut bytes = sample_messages()[0].to_bytes();
        bytes.push(0xFF); // trailing byte
        assert!(DkgWireMessage::from_bytes(&bytes).is_err());
        assert!(DkgWireMessage::from_bytes(&[0, 0, 0, 0, 255]).is_err()); // truncated field
    }
}
