//! Transport-layer errors for the node and enclave channel.

use std::fmt;
use thiserror::Error;

use crate::tee_call::TeeAttestationError;

/// Domain-supplied metric label and display context for an application-layer enclave error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppErrorDescriptor {
    /// Stable Prometheus label for `outbe_tee_client_request_errors_total{class}`.
    pub metric_class: &'static str,
    /// Human-readable subject prefix for display formatting.
    pub context: &'static str,
}

impl AppErrorDescriptor {
    /// Construct a domain error descriptor with a stable metric label and display context.
    #[must_use]
    pub const fn new(metric_class: &'static str, context: &'static str) -> Self {
        Self {
            metric_class,
            context,
        }
    }
}

/// Errors returned by the host-side enclave transport and session layer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Socket read or write failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Socket operation exceeded the configured timeout.
    #[error("enclave io timeout during {operation} after {timeout_secs}s")]
    IoTimeout {
        /// Name of the timed-out transport phase.
        operation: &'static str,
        /// Timeout threshold in seconds.
        timeout_secs: u64,
    },

    /// Wire frame length exceeded the protocol ceiling.
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),

    /// Message serialization or canonical decoding failed.
    #[error("codec error: {0}")]
    Codec(String),

    /// Noise cipher operation failed.
    #[error("noise error: {0}")]
    Noise(String),

    /// Noise handshake or session authorization failed.
    #[error("handshake error: {0}")]
    Handshake(String),

    /// Quote or intent attestation check failed.
    #[error("attestation verification failed: {0}")]
    Attestation(String),

    /// Application-layer payload attestation signature check failed.
    #[error("{}: {source}", descriptor.context)]
    AppAttestation {
        /// Metric counter classification and display context (never used for control-flow branching).
        descriptor: AppErrorDescriptor,
        /// Structured attestation verification failure cause.
        #[source]
        source: TeeAttestationError,
    },

    /// DCAP verification stream or response check failed.
    #[error("DCAP verification channel invalid: {0}")]
    DcapVerification(String),

    /// Attestation preimage JSON did not serialize. The source stays structured.
    #[error("attestation preimage serialization failed: {0}")]
    AttestationPreimageSerialization(#[from] serde_json::Error),

    /// Enclave returned a response variant that does not match the request.
    #[error("unexpected response from enclave")]
    UnexpectedResponse,

    /// Enclave rejected the request with an explicit error message.
    #[error("enclave returned error: {0}")]
    EnclaveError(String),

    /// Application-layer operation or state source was deterministically rejected.
    #[error("{}: {reason}", descriptor.context)]
    AppRejected {
        /// Metric counter classification and display context (never used for control-flow branching).
        descriptor: AppErrorDescriptor,
        /// Rejection reason string.
        reason: String,
    },

    /// Reconnected enclave identity differed from the pinned identity.
    #[error("enclave identity mismatch after reconnect: {0}")]
    IdentityMismatch(String),

    /// Session was permanently revoked after an identity mismatch.
    #[error("enclave session permanently revoked: {0}")]
    SessionRevoked(&'static str),

    /// Enclave sidecar was not configured or not ready.
    #[error("enclave unavailable: {0}")]
    Unavailable(String),
}

const _: () = assert!(std::mem::size_of::<TransportError>() <= 112);

impl TransportError {
    /// Construct a [`Self::Codec`] error from a displayable value.
    pub fn codec(error: impl fmt::Display) -> Self {
        Self::Codec(error.to_string())
    }

    /// Construct a [`Self::Noise`] error from a displayable value.
    pub fn noise(error: impl fmt::Display) -> Self {
        Self::Noise(error.to_string())
    }

    /// Construct a [`Self::Noise`] error from a debug-formatted value.
    pub fn noise_debug(error: snow::Error) -> Self {
        super::TeeClientVerificationError::NoiseParams(error).into()
    }

    /// Construct a [`Self::Handshake`] error from a displayable value.
    pub fn handshake(error: impl fmt::Display) -> Self {
        Self::Handshake(error.to_string())
    }

    /// Construct a [`Self::Attestation`] error from a displayable value.
    pub fn attestation(error: impl fmt::Display) -> Self {
        Self::Attestation(error.to_string())
    }

    /// Construct a quote-parse [`Self::Attestation`] error.
    pub fn quote_parse(error: super::QuoteParseError) -> Self {
        super::TeeClientVerificationError::QuoteParse(error).into()
    }

    /// Construct a structured application-layer attestation failure, preserving
    /// [`Self::AttestationPreimageSerialization`] when canonical JSON serialization fails.
    pub fn app_attestation(descriptor: AppErrorDescriptor, source: TeeAttestationError) -> Self {
        match source {
            TeeAttestationError::CanonicalJson(err) => Self::AttestationPreimageSerialization(err),
            other => Self::AppAttestation {
                descriptor,
                source: other,
            },
        }
    }

    /// Construct an application-layer deterministic rejection with a typed error descriptor.
    pub fn app_rejected(descriptor: AppErrorDescriptor, reason: impl fmt::Display) -> Self {
        Self::AppRejected {
            descriptor,
            reason: reason.to_string(),
        }
    }

    /// Construct a [`Self::DcapVerification`] error from a displayable value.
    pub fn dcap_verification(error: impl fmt::Display) -> Self {
        Self::DcapVerification(error.to_string())
    }

    /// Construct a [`Self::EnclaveError`] error from a displayable value.
    pub fn enclave_error(error: impl fmt::Display) -> Self {
        Self::EnclaveError(error.to_string())
    }

    /// Construct a [`Self::Unavailable`] error when a panic poisons the snapshot lock.
    pub fn snapshot_lock_poisoned<T>(_: T) -> Self {
        Self::Unavailable(super::CallContextError::SnapshotLockPoisoned.to_string())
    }

    /// Construct a [`Self::Codec`] error when a NodeHost file length overflows.
    pub fn node_host_length_overflow<T>(label: &'static str) -> impl FnOnce(T) -> Self {
        move |_| super::NodeHostRecordError::LengthOverflow { label }.into()
    }

    /// Construct a [`Self::IdentityMismatch`] error from a reason and detail.
    pub fn identity_mismatch(reason: &'static str, detail: impl fmt::Display) -> Self {
        super::TeeClientVerificationError::ReconnectIdentityMismatch {
            reason,
            detail: detail.to_string(),
        }
        .into()
    }

    /// Return true when the connection is broken or desynchronized.
    ///
    /// A bounded reconnect and one retry can recover from these variants:
    /// - `Io` and `IoTimeout`: socket-level fault.
    /// - `Noise`: the initiator nonce advanced, so the cipher state is unusable.
    /// - `FrameTooLarge`: corrupt stream on read.
    ///
    /// This method excludes other variants because a fresh connection repeats the same failure.
    pub fn is_connection_fault(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::IoTimeout { .. } | Self::Noise(_) | Self::FrameTooLarge(_)
        )
    }

    /// Return true when the fault lies with this node's enclave, session, or socket.
    pub fn is_node_local(&self) -> bool {
        matches!(
            self,
            Self::Io(_)
                | Self::IoTimeout { .. }
                | Self::Noise(_)
                | Self::FrameTooLarge(_)
                | Self::Handshake(_)
                | Self::SessionRevoked(_)
                | Self::IdentityMismatch(_)
                | Self::Unavailable(_)
        )
    }

    /// Return the stable label for the `outbe_tee_client_request_errors_total{class}` counter.
    pub fn metric_class(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::IoTimeout { .. } => "io_timeout",
            Self::FrameTooLarge(_) => "frame_too_large",
            Self::Codec(_) => "codec",
            Self::Noise(_) => "noise",
            Self::Handshake(_) => "handshake",
            Self::Attestation(_) => "attestation",
            Self::AppAttestation { descriptor, .. } => descriptor.metric_class,
            Self::DcapVerification(_) => "dcap_verification",
            Self::AttestationPreimageSerialization(_) => "attestation_preimage_serialization",
            Self::UnexpectedResponse => "unexpected_response",
            Self::EnclaveError(_) => "enclave_error",
            Self::AppRejected { descriptor, .. } => descriptor.metric_class,
            Self::IdentityMismatch(_) => "identity_mismatch",
            Self::SessionRevoked(_) => "session_revoked",
            Self::Unavailable(_) => "unavailable",
        }
    }
}
