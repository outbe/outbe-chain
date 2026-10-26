//! Error types for the Governance precompile.

use crate::precompile::IGovernance;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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

    #[error("canon text is installed: change it through an approved vote proposal")]
    CanonRequiresVote,
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<GovernanceError> for PrecompileError {
    fn from(value: GovernanceError) -> Self {
        use IGovernance as I;
        match value {
            GovernanceError::NotAuthorized => revert_with(I::NotAuthorized {}),
            GovernanceError::NotAuthor => revert_with(I::NotAuthor {}),
            GovernanceError::ProposalNotFound => revert_with(I::ProposalNotFound {}),
            GovernanceError::InvalidStatusTransition => revert_with(I::InvalidStatusTransition {}),
            GovernanceError::InvalidStatus => revert_with(I::InvalidStatus {}),
            GovernanceError::TextNotEditableInStatus => revert_with(I::TextNotEditableInStatus {}),
            GovernanceError::EmptyText => revert_with(I::EmptyText {}),
            GovernanceError::TextTooLarge { len, max } => revert_with(I::TextTooLarge {
                len: u64::try_from(len).unwrap_or(u64::MAX),
                max: u64::try_from(max).unwrap_or(u64::MAX),
            }),
            GovernanceError::InvalidDiffBase => revert_with(I::InvalidDiffBase {}),
            GovernanceError::InvalidPayload => revert_with(I::InvalidPayload {}),
            GovernanceError::InvalidProposalPayload => revert_with(I::InvalidProposalPayload {}),
            GovernanceError::AuthorIndexFull => revert_with(I::AuthorIndexFull {}),
            GovernanceError::ProposalIdExhausted => revert_with(I::ProposalIdExhausted {}),
            GovernanceError::CanonRequiresVote => revert_with(I::CanonRequiresVote {}),
        }
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
