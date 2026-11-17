//! Errors at the resident enclave dispatch and host filesystem boundaries.

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

/// Errors returned by resident dispatch, verification, and host I/O.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Direct resident/domain failure with its structured cause intact.
    #[error("in-process TEE dispatch failed: {0}")]
    InProcess(#[from] crate::in_process_dispatch::InProcessDispatchError),
    /// Host I/O failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Message serialization or canonical decoding failed.
    #[error("codec error: {0}")]
    Codec(String),

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

    /// Resident verification rejected the response or request binding.
    #[error("DCAP verification channel invalid: {0}")]
    Verification(#[from] super::TeeVerificationError),

    /// A finalized upgrade proof failed its size or record bounds.
    #[error("enclave returned error: {0}")]
    UpgradeProof(#[from] super::UpgradeTransferError),

    /// A NodeHost filesystem or record invariant failed.
    #[error("codec error: {0}")]
    NodeHostRecord(#[source] super::NodeHostRecordError),

    /// A NodeHost attestation record omitted its key-ready proof.
    #[error("attestation verification failed: {0}")]
    NodeHostAttestation(#[source] super::NodeHostRecordError),

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

    /// Resident execution or its context is unavailable.
    #[error("enclave unavailable: {0}")]
    Unavailable(String),
}

const _: () = assert!(std::mem::size_of::<TransportError>() <= 112);

impl TransportError {
    /// Construct a [`Self::Codec`] error from a displayable value.
    pub fn codec(error: impl fmt::Display) -> Self {
        Self::Codec(error.to_string())
    }

    /// Construct a [`Self::Attestation`] error from a displayable value.
    pub fn attestation(error: impl fmt::Display) -> Self {
        Self::Attestation(error.to_string())
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

    /// Return true when the fault lies with this node's resident context or local infrastructure.
    pub fn is_node_local(&self) -> bool {
        matches!(
            self,
            Self::InProcess(
                crate::InProcessDispatchError::Resident(_)
                    | crate::InProcessDispatchError::ContextNotInstalled
            ) | Self::Io(_)
                | Self::Unavailable(_)
        )
    }

    /// Return the stable label for the `outbe_tee_client_request_errors_total{class}` counter.
    pub fn metric_class(&self) -> &'static str {
        match self {
            Self::InProcess(crate::InProcessDispatchError::ContextNotInstalled) => "unavailable",
            Self::InProcess(_) => "in_process",
            Self::Io(_) => "io",

            Self::Codec(_) => "codec",

            Self::Attestation(_) => "attestation",
            Self::AppAttestation { descriptor, .. } => descriptor.metric_class,
            Self::Verification(_) => "dcap_verification",
            Self::NodeHostRecord(_) => "codec",
            Self::NodeHostAttestation(_) => "attestation",
            Self::AttestationPreimageSerialization(_) => "attestation_preimage_serialization",
            Self::UnexpectedResponse => "unexpected_response",
            Self::EnclaveError(_) | Self::UpgradeProof(_) => "enclave_error",
            Self::AppRejected { descriptor, .. } => descriptor.metric_class,

            Self::Unavailable(_) => "unavailable",
        }
    }
}
