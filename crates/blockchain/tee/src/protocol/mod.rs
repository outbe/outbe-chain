//! Typed resident dispatch messages and the public health snapshot schema.

use alloy_primitives::B256;

mod enclave_request;
mod enclave_response;

pub use enclave_request::EnclaveRequest;
pub use enclave_response::{
    eip191_hash, EnclaveHealthStatusV1, EnclaveHeapUsageV1, EnclaveRequestClassCountsV1,
    EnclaveRequestCountsV1, EnclaveResponse,
};

/// One DKG participant's ceremony-scoped identity. `enc_sig` authenticates:
/// - the full network binding
/// - the ceremony id and round
/// - the exact participant-set hash
/// - the X25519 share-recipient key
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParticipantAnnounce {
    /// Encoded TEE-BLS public key (the participant's DKG identity).
    pub bls_pub: Vec<u8>,
    /// Announced X25519 share-encryption public key.
    pub enc_pub: [u8; 32],
    pub ceremony_id: B256,
    pub round: u64,
    pub participant_set_hash: B256,
    /// TEE-BLS signature over the complete ceremony-scoped announcement.
    pub enc_sig: Vec<u8>,
}
