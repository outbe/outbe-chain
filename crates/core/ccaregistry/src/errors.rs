//! Error types for the CCA registry.
use crate::precompile::ICcaRegistry;
use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolError;
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
    #[error("CCA is not active: {cca} ({state:?})")]
    CcaNotActive {
        cca: Address,
        state: ICcaRegistry::State,
    },
    #[error("CCA unbond is pending")]
    UnbondPending,
    #[error("CCA has no pending unbond")]
    NoUnbond,
    #[error("CCA unbond cooldown has not completed")]
    Cooldown,
    #[error("invalid CCA state: {0}")]
    InvalidState(u8),
    #[error("CCA arithmetic overflow or underflow")]
    Arithmetic,
    #[error("CCA address must be nonzero")]
    ZeroAddress,
    #[error("CCA active set is full")]
    ActiveSetFull,
    #[error("CCA registry balance is below the bonded total")]
    CustodyShortfall,
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<CcaError> for PrecompileError {
    fn from(error: CcaError) -> Self {
        use ICcaRegistry as I;
        match error {
            CcaError::InvalidAmount => revert_with(I::InvalidAmount {}),
            CcaError::InvalidName => revert_with(I::InvalidName {}),
            CcaError::NotRegistered => revert_with(I::NotRegistered {}),
            CcaError::CcaNotActive { cca, state } => revert_with(I::CcaNotActive { cca, state }),
            CcaError::UnbondPending => revert_with(I::UnbondPending {}),
            CcaError::NoUnbond => revert_with(I::NoUnbond {}),
            CcaError::Cooldown => revert_with(I::Cooldown {}),
            CcaError::InvalidState(value) => revert_with(I::InvalidState { value }),
            CcaError::Arithmetic => revert_with(I::Arithmetic {}),
            CcaError::ZeroAddress => revert_with(I::ZeroAddress {}),
            CcaError::ActiveSetFull => revert_with(I::ActiveSetFull {}),
            CcaError::CustodyShortfall => revert_with(I::CustodyShortfall {}),
        }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CcaFatalError {
    #[error("CCA active index entry missing")]
    ActiveIndexEntryMissing,
}

impl From<CcaFatalError> for PrecompileError {
    fn from(error: CcaFatalError) -> Self {
        Self::Revert(error.to_string())
    }
}
