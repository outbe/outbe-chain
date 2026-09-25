//! CCA transition and arithmetic errors.
use alloy_primitives::Address;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CcaError {
    #[error("CCA amount must be positive")]
    InvalidAmount,
    #[error("CCA name must be nonempty")]
    InvalidName,
    #[error("CCA is not registered")]
    NotRegistered,
    #[error("CCA is not active: {0}")]
    CcaNotActive(Address),
    #[error("CCA unbond is pending")]
    UnbondPending,
    #[error("CCA has no pending unbond")]
    NoUnbond,
    #[error("CCA unbond cooldown has not completed")]
    Cooldown,
    #[error("CCA has no claimable rewards")]
    NoRewards,
    #[error("CCA insufficient claimable rewards")]
    InsufficientRewards,
    #[error("invalid CCA state: {0}")]
    InvalidState(u8),
    #[error("CCA arithmetic overflow or underflow")]
    Arithmetic,
    #[error("CCA address must be nonzero")]
    ZeroAddress,
}

impl From<CcaError> for PrecompileError {
    fn from(error: CcaError) -> Self {
        Self::Revert(error.to_string())
    }
}
