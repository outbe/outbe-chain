//! Typed error definitions for the CodeRegistry system precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the CodeRegistry runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodeRegistryError {
    #[error("Radicle repository id must be non-zero")]
    ZeroRepositoryId,
    #[error("Radicle repository is already registered")]
    AlreadyRegistered,
    #[error("Radicle repository registry capacity {maximum} reached")]
    CapacityReached { maximum: u32 },
    #[error("Radicle repository index {index} is outside count {count}")]
    IndexOutOfBounds { index: u32, count: u32 },
    #[error("Radicle repository storage key has a non-zero tail")]
    NonZeroStorageKeyTail,
    #[error("Radicle repository index {index} is inconsistent")]
    InconsistentRepositoryIndex { index: u32 },
    #[error("invalid or noncanonical Radicle patch Vote payload")]
    InvalidPatchPayload,
    #[error("Radicle patch Vote repository is not registered")]
    UnknownRepository,
    #[error("Radicle patch approval does not match the stored Vote proposal")]
    ApprovalProposalMismatch,
    #[error("Radicle patch approval is immutable")]
    ImmutableApproval,
    #[error("invalid Radicle publication: {0}")]
    InvalidPublication(&'static str),
}

impl From<CodeRegistryError> for PrecompileError {
    fn from(error: CodeRegistryError) -> Self {
        Self::Revert(error.to_string())
    }
}
