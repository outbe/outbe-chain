//! Reshare authority endorsement message contract (public; no secret material).
//!
//! The prior (outgoing) committee threshold-signs [`reshare_endorsement_message`]
//! to authorize an incoming committee's TEE re-registrations. The enclave produces
//! its partial over this message (`bin/outbe-tee-enclave`); the begin-zone
//! `BoundaryOutcome` handler verifies the recovered group signature against the
//! stored prior group public key. Both sides MUST use this exact namespace +
//! message so signing and verification agree — hence it lives in this shared
//! message-contract crate, not in the enclave binary.

use alloy_primitives::{keccak256, B256};

/// Namespace for the reshare authority endorsement threshold signature. Distinct
/// from the offer-key namespace so an endorsement partial can never be replayed as
/// an offer-key partial.
pub const TEE_ENDORSE_NAMESPACE: &[u8] = b"outbe-tee-reshare-endorse";

/// Domain tag bound into the endorsed commitment.
const ENDORSE_DOMAIN: &[u8] = b"outbe/tee/reshare-endorse/v1";

/// The commitment a prior committee endorses to authorize a reshared committee:
/// `keccak256(ENDORSE_DOMAIN || chain_id || new_committee_set_hash || offer_pub)`.
/// Binds the chain, the canonical V2 committee identity of the incoming set, and
/// the preserved offer key.
pub fn reshare_endorsement_message(
    chain_id: B256,
    new_committee_set_hash: B256,
    tribute_offer_public: [u8; 32],
) -> B256 {
    let mut buf =
        Vec::with_capacity(ENDORSE_DOMAIN.len() + B256::len_bytes() + B256::len_bytes() + 32);
    buf.extend_from_slice(ENDORSE_DOMAIN);
    buf.extend_from_slice(chain_id.as_slice());
    buf.extend_from_slice(new_committee_set_hash.as_slice());
    buf.extend_from_slice(&tribute_offer_public);
    keccak256(&buf)
}
