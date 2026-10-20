//! Typed errors for enclave call-context resolution and installation.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Error message type returned by an installed call-context provider callback.
pub type CallContextProviderError = String;

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
