//! Transport-layer errors for the node <-> enclave channel.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("enclave io timeout during {operation} after {timeout_secs}s")]
    IoTimeout {
        operation: &'static str,
        timeout_secs: u64,
    },

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

    #[error("promis-op attestation signature invalid: {0}")]
    PromisOpAttestation(String),

    #[error("DCAP verification channel invalid: {0}")]
    DcapVerification(String),

    #[error("fidelity attestation signature invalid: {0}")]
    FidelityAttestation(String),

    #[error("unexpected response from enclave")]
    UnexpectedResponse,

    #[error("enclave returned error: {0}")]
    EnclaveError(String),

    #[error("NOD materialization rejected: {0}")]
    NodMaterializationRejected(String),

    #[error("NOD materialization capacity exceeded: {0}")]
    NodMaterializationCapacityExceeded(String),

    #[error("encrypted NOD mint rejected: {0}")]
    NodMintRejected(String),

    #[error("enclave identity mismatch after reconnect: {0}")]
    IdentityMismatch(String),

    #[error("enclave session permanently revoked: {0}")]
    SessionRevoked(&'static str),
}

impl TransportError {
    /// True when the error means the connection itself is broken or
    /// desynchronized, so a bounded reconnect + one retry may help:
    /// - `Io` / `IoTimeout`: socket-level fault.
    /// - `Noise`: after any failed round-trip, the initiator nonce advanced.
    ///   The cipher state is then unusable, whatever the cause.
    /// - `FrameTooLarge` on read: corrupt stream.
    ///
    /// Deliberately excluded:
    /// - `Handshake` - local policy rejections also produce it
    ///   (e.g. `AuthorizeRemoteSessionV1` misuse), not only transport.
    /// - `EnclaveError` - the enclave answered. The connection is healthy and
    ///   the answer is deterministic.
    /// - `Codec` / `UnexpectedResponse` / attestation errors - post-decryption
    ///   protocol or enclave faults a fresh connection deterministically repeats.
    pub fn is_connection_fault(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::IoTimeout { .. } | Self::Noise(_) | Self::FrameTooLarge(_)
        )
    }

    /// Stable label for the `outbe_tee_request_errors_total{class}` counter.
    pub fn metric_class(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::IoTimeout { .. } => "io_timeout",
            Self::FrameTooLarge(_) => "frame_too_large",
            Self::Codec(_) => "codec",
            Self::Noise(_) => "noise",
            Self::Handshake(_) => "handshake",
            Self::Attestation(_) => "attestation",
            Self::TributeOfferAttestation(_) => "tribute_offer_attestation",
            Self::GratisOpAttestation(_) => "gratis_op_attestation",
            Self::PromisOpAttestation(_) => "promis_op_attestation",
            Self::DcapVerification(_) => "dcap_verification",
            Self::FidelityAttestation(_) => "fidelity_attestation",
            Self::UnexpectedResponse => "unexpected_response",
            Self::EnclaveError(_) => "enclave_error",
            Self::NodMaterializationRejected(_) => "nod_materialization_rejected",
            Self::NodMaterializationCapacityExceeded(_) => "nod_materialization_capacity",
            Self::NodMintRejected(_) => "nod_mint_rejected",
            Self::IdentityMismatch(_) => "identity_mismatch",
            Self::SessionRevoked(_) => "session_revoked",
        }
    }
}
