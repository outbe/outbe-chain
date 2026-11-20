//! Typed errors for enclave call-context resolution and installation.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Typed error returned by an installed call-context provider callback.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct CallContextProviderError {
    /// Structured error returned by the call-context provider.
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl CallContextProviderError {
    /// Wrap a typed provider failure while preserving its `#[source]` chain.
    pub fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(error),
        }
    }
}

/// Errors returned by [`crate::call_context`] operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum CallContextError {
    /// Stored protocol version exceeded its integer width.
    #[error("enclave call context integer overflow")]
    IntegerOverflow,

    /// A panic poisoned the process-wide snapshot lock.
    #[error("enclave snapshot lock poisoned")]
    SnapshotLockPoisoned,

    /// Call-context provider was already installed.
    #[error("enclave context provider already installed")]
    ProviderAlreadyInstalled,
}

impl CallContextError {
    /// Return the static error string for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IntegerOverflow => "enclave call context integer overflow",
            Self::SnapshotLockPoisoned => "enclave snapshot lock poisoned",
            Self::ProviderAlreadyInstalled => "enclave context provider already installed",
        }
    }

    /// Map a poisoned lock error to [`Self::SnapshotLockPoisoned`].
    pub fn snapshot_lock_poisoned<T>(_: T) -> Self {
        Self::SnapshotLockPoisoned
    }

    /// Map a duplicate installation error to [`Self::ProviderAlreadyInstalled`].
    pub fn provider_already_installed<T>(_: T) -> Self {
        Self::ProviderAlreadyInstalled
    }
}

impl std::ops::Deref for CallContextError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for CallContextError {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for CallContextError {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl From<CallContextError> for &'static str {
    fn from(error: CallContextError) -> Self {
        error.as_str()
    }
}

impl From<CallContextError> for String {
    fn from(error: CallContextError) -> Self {
        error.as_str().to_owned()
    }
}

impl From<CallContextError> for PrecompileError {
    fn from(error: CallContextError) -> Self {
        match error {
            CallContextError::IntegerOverflow => Self::Revert(error.as_str().into()),
            CallContextError::SnapshotLockPoisoned | CallContextError::ProviderAlreadyInstalled => {
                Self::Fatal(error.as_str().into())
            }
        }
    }
}

/// A typed call was routed to the wrong recordable lane.
#[derive(Debug, Error)]
pub(crate) enum TypedCallRouteError {
    /// `tee_consensus_typed_call` received a session-only call.
    #[error("non-recordable call routed to tee_consensus_typed_call")]
    NonRecordableConsensus,
    /// `tee_session_typed_call` received a witness-recorded call.
    #[error("recordable call routed to non-consensus tee_session_typed_call")]
    RecordableSession,
}

/// A typed witness call cannot run against the resident enclave.
#[derive(Debug, Error)]
pub(crate) enum WitnessAvailabilityError {
    /// The in-memory transport panicked before it returned a value.
    #[error("local enclave transport panicked")]
    TransportPanicked,
    /// A block policy is installed and the call has no witness scope.
    #[error("private execution requires an explicit witness scope")]
    MissingWitnessScope,
    /// Replay-only policy refused a direct resident call.
    #[error("PrivateExecutionUnavailable")]
    PrivateExecutionUnavailable,
    /// Recorded witness bytes differ from the live encoding.
    #[error("block witness fault: SemanticMismatch")]
    SemanticMismatch,
    /// The witness tape has no entry for this call.
    #[error("block witness fault: TapeInvalid")]
    TapeInvalid,
    /// The resident answer cannot be committed to the tape.
    #[error("block witness fault: LocalUnavailable")]
    LocalUnavailable,
    /// The witness-state mutex was poisoned.
    #[error("witness state lock poisoned")]
    LockPoisoned,
}

/// A user candidate already failed and must not call the resident again.
#[derive(Debug, Error)]
#[error("witness candidate already rejected")]
pub(crate) struct WitnessCandidateRejected;

/// Enclave returned an explicit error response message.
#[derive(Debug, Error)]
#[error("{0}")]
pub(crate) struct EnclaveResponseError(pub(crate) String);
