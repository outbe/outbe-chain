//! Errors at the resident enclave dispatch and host filesystem boundaries.

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

    /// Enclave rejected the request. The typed cause stays attached.
    #[error("enclave returned error: {source}")]
    EnclaveCause {
        /// Structured enclave rejection.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Application-layer operation or state source was deterministically rejected.
    #[error("{}: {source}", descriptor.context)]
    AppRejected {
        /// Metric counter classification and display context (never used for control-flow branching).
        descriptor: AppErrorDescriptor,
        /// Structured rejection cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Resident execution or its context is unavailable.
    #[error("enclave unavailable: {0}")]
    Unavailable(String),

    /// Message serialization failed. The typed cause stays attached.
    #[error("codec error: {source}")]
    CodecCause {
        /// Structured codec failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Quote or intent attestation check failed. The typed cause stays attached.
    #[error("attestation verification failed: {source}")]
    AttestationCause {
        /// Structured attestation failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A call-context or witness-scope fault made resident execution unavailable.
    #[error("enclave unavailable: {source}")]
    UnavailableCause {
        /// Structured reason resident execution cannot proceed.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

const _: () = assert!(std::mem::size_of::<TransportError>() <= 112);

impl TransportError {
    /// Construct a codec failure that keeps the typed cause.
    pub fn codec(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::CodecCause {
            source: Box::new(error),
        }
    }

    /// Construct an attestation failure that keeps the typed cause.
    pub fn attestation(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::AttestationCause {
            source: Box::new(error),
        }
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
    pub fn app_rejected(
        descriptor: AppErrorDescriptor,
        reason: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::AppRejected {
            descriptor,
            source: reason.into(),
        }
    }

    /// Construct an enclave rejection that keeps the typed cause.
    pub fn enclave_error(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::EnclaveCause {
            source: Box::new(error),
        }
    }

    /// Construct an unavailable failure that keeps the typed cause.
    pub fn unavailable(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::UnavailableCause {
            source: Box::new(error),
        }
    }

    /// Construct an unavailable error when a panic poisons the snapshot lock.
    pub fn snapshot_lock_poisoned<T>(_: T) -> Self {
        Self::unavailable(super::CallContextError::SnapshotLockPoisoned)
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
                    | crate::InProcessDispatchError::OfferKeyNotReady(_)
            ) | Self::Io(_)
                | Self::Unavailable(_)
                | Self::UnavailableCause { .. }
        )
    }

    /// Return the stable label for the `outbe_tee_client_request_errors_total{class}` counter.
    pub fn metric_class(&self) -> &'static str {
        match self {
            Self::InProcess(
                crate::InProcessDispatchError::ContextNotInstalled
                | crate::InProcessDispatchError::OfferKeyNotReady(_),
            ) => "unavailable",
            Self::InProcess(_) => "in_process",
            Self::Io(_) => "io",

            Self::Codec(_) | Self::CodecCause { .. } => "codec",

            Self::Attestation(_) | Self::AttestationCause { .. } => "attestation",
            Self::AppAttestation { descriptor, .. } => descriptor.metric_class,
            Self::Verification(_) => "dcap_verification",
            Self::NodeHostRecord(_) => "codec",
            Self::NodeHostAttestation(_) => "attestation",
            Self::AttestationPreimageSerialization(_) => "attestation_preimage_serialization",
            Self::UnexpectedResponse => "unexpected_response",
            Self::EnclaveError(_) | Self::EnclaveCause { .. } | Self::UpgradeProof(_) => {
                "enclave_error"
            }
            Self::AppRejected { descriptor, .. } => descriptor.metric_class,

            Self::Unavailable(_) | Self::UnavailableCause { .. } => "unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn constructors_keep_typed_sources() {
        let codec = TransportError::codec(std::io::Error::other("disk"));
        assert_eq!(codec.metric_class(), "codec");
        assert!(!codec.is_node_local());
        assert!(Error::source(&codec)
            .and_then(|cause| cause.downcast_ref::<std::io::Error>())
            .is_some());

        let attestation = TransportError::attestation(TeeAttestationError::NonCanonicalPayload);
        assert_eq!(attestation.metric_class(), "attestation");
        assert!(Error::source(&attestation).is_some());

        let poisoned = TransportError::snapshot_lock_poisoned(());
        assert!(poisoned.is_node_local());
        assert_eq!(poisoned.metric_class(), "unavailable");
        assert!(matches!(
            Error::source(&poisoned)
                .unwrap()
                .downcast_ref::<crate::CallContextError>(),
            Some(crate::CallContextError::SnapshotLockPoisoned)
        ));

        let enclave = TransportError::enclave_error(std::io::Error::other("refused"));
        assert_eq!(enclave.metric_class(), "enclave_error");
        assert!(!enclave.is_node_local());
        assert!(Error::source(&enclave)
            .and_then(|cause| cause.downcast_ref::<std::io::Error>())
            .is_some());
    }

    #[derive(Debug)]
    struct Rejected;

    impl std::fmt::Display for Rejected {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("nope")
        }
    }

    impl std::error::Error for Rejected {}

    #[test]
    fn app_rejected_keeps_the_typed_source() {
        let error =
            TransportError::app_rejected(AppErrorDescriptor::new("class", "context"), Rejected);
        assert!(Error::source(&error)
            .and_then(|cause| cause.downcast_ref::<Rejected>())
            .is_some());
        assert_eq!(error.metric_class(), "class");
        assert!(!error.is_node_local());
        let literal =
            TransportError::app_rejected(AppErrorDescriptor::new("class", "context"), "x");
        assert!(Error::source(&literal).is_some());
    }
}
