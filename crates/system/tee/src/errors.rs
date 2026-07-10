//! Transport-layer errors for the node <-> enclave channel.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("noise error: {0}")]
    Noise(String),

    #[error("handshake error: {0}")]
    Handshake(String),

    #[error("attestation verification failed: {0}")]
    Attestation(String),

    #[error("offer attestation signature invalid: {0}")]
    TributeOfferAttestation(String),

    #[error("gratis-op attestation signature invalid: {0}")]
    GratisOpAttestation(String),

    #[error("unexpected response from enclave")]
    UnexpectedResponse,

    #[error("enclave returned error: {0}")]
    EnclaveError(String),
}
