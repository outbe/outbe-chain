//! Validate remote session ticket fields before admission.

use crate::keys::EnclaveKeys;
use alloy_primitives::B256;

#[derive(Clone, Copy, Debug)]
pub struct RemoteSessionAuthorization {
    pub ticket_id: B256,
    pub initiator_static_x25519: [u8; 32],
    pub responder_static_x25519: [u8; 32],
    pub deadline: u64,
    pub finalized_block_hash: B256,
}

impl RemoteSessionAuthorization {
    pub(super) fn validate_responder(&self, keys: &EnclaveKeys) -> Result<(), String> {
        let ticket_is_valid = !self.ticket_id.is_zero() && !self.finalized_block_hash.is_zero();
        let participants_are_valid =
            self.initiator_static_x25519 != [0; 32] && self.responder_static_x25519 != [0; 32];
        if !ticket_is_valid || !participants_are_valid {
            return Err("remote session authorization is malformed".into());
        }
        if self.responder_static_x25519 != keys.noise_public() {
            return Err("remote session targets another Noise responder".into());
        }
        Ok(())
    }

    pub(super) fn ensure_live_at(&self, now: u64) -> Result<(), String> {
        if self.deadline <= now {
            return Err("remote session authorization is expired".into());
        }
        Ok(())
    }
}
