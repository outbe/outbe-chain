//! Neutral wire-protocol types for the node <-> enclave channel.
//!
//! These types are the message contract shared by the host (`outbe-tee`) and
//! the enclave (`outbe-tee-enclave`). They carry **no secret material** and no
//! cryptographic logic. They define only the shape of requests and responses.
//!
//! Transport: length-prefixed framing over UDS/TCP, wrapped in a Noise-IK
//! transport (payload layer). Production first exposes only an initialization
//! challenge. A node-signed manifest installs one persistent `NodeHost`
//! initiator. The enclave accepts every later command, including quote
//! generation, only after Noise message 1 authenticates that initiator.
//! `GetQuote` exists only for the separate development transport.

use alloy_primitives::B256;

/// Canonical DirectDev onboarding context used by the typed enclave request.
pub type GramineDirectDevOnboardingContextV1 = crate::dcap_protocol::DcapOnboardingContextV1;

mod enclave_request;
mod enclave_response;

pub use enclave_request::EnclaveRequest;
pub use enclave_response::{
    eip191_hash, EnclaveHealthStatusV1, EnclaveHeapUsageV1, EnclaveRequestClassCountsV1,
    EnclaveRequestCountsV1, EnclaveResponse,
};

/// Host-enclave connection lane for opaque application-layer [`EnclaveRequest::Payload`] calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PayloadLane {
    /// Block execution and authenticated NodeHost consensus/RPC read-only calls.
    Execution,
    /// Background canary liveness/readiness probes.
    Canary,
    /// Off-chain user account key-derivation RPC queries.
    AccountQuery,
}

/// How a request participates in the block's enclave witness tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsensusWitnessKind {
    Recorded,
    StreamFrame,
    NonConsensus,
}

/// Hard cap for the deterministic registry onboarding artifact. The current
/// X25519/nonce/AEAD envelope is substantially smaller. This cap prevents a
/// malformed enclave response from creating an unbounded consensus log.
pub const MAX_ONBOARDING_ARTIFACT_BYTES: usize = 512;

/// Minimum canonical framing: the committed 32-byte offer public key plus the
/// ephemeral public key, nonce and authenticated ciphertext framing.
pub const MIN_ONBOARDING_ARTIFACT_BYTES: usize = 60;

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
