//! Error types for the Governance precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Revert errors that the Governance runtime returns.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum GovernanceError {
    #[error("not authorized")]
    NotAuthorized,

    #[error("not the proposal author")]
    NotAuthor,

    #[error("proposal not found")]
    ProposalNotFound,

    #[error("invalid status transition")]
    InvalidStatusTransition,

    #[error("invalid status value")]
    InvalidStatus,

    #[error("text is not editable in the current status")]
    TextNotEditableInStatus,

    #[error("text must not be empty")]
    EmptyText,

    #[error("text exceeds the maximum size ({len} > {max})")]
    TextTooLarge { len: usize, max: usize },

    #[error("invalid diff base (expected 0 = canon or 1 = meta-canon)")]
    InvalidDiffBase,

    #[error("invalid vote payload")]
    InvalidPayload,

    #[error("invalid proposal payload")]
    InvalidProposalPayload,

    #[error("author proposal index is full")]
    AuthorIndexFull,

    #[error("proposal id sequence is exhausted")]
    ProposalIdExhausted,
}

impl From<GovernanceError> for PrecompileError {
    fn from(value: GovernanceError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum GovernanceFatalError {
    #[error("stored Governance proposal payload is invalid")]
    InvalidStoredProposalPayload,
}

impl From<GovernanceFatalError> for PrecompileError {
    fn from(value: GovernanceFatalError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
