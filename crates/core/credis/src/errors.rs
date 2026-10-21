//! Module-local error types for the Credis contract.
//!
//! Errors that are not credis-specific (out-of-gas, generic revert) come from
//! `outbe_primitives::error::PrecompileError`.

use crate::precompile::ICredis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredisError {
    #[error("Credis not found")]
    CredisNotFound,
    #[error("invalid Credis state value: {0}")]
    InvalidStateValue(u8),
    #[error("credis arithmetic overflow")]
    ArithmeticOverflow,
    #[error("index out of bounds")]
    IndexOutOfBounds,
    #[error("Credis is non-transferable")]
    NonTransferable,
    #[error("unknown credis profile selector: {0}")]
    UnknownProfileSelector(u8),
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<CredisError> for PrecompileError {
    fn from(err: CredisError) -> Self {
        use ICredis as I;
        match err {
            CredisError::CredisNotFound => revert_with(I::CredisNotFound {}),
            CredisError::InvalidStateValue(value) => revert_with(I::InvalidStateValue { value }),
            CredisError::ArithmeticOverflow => revert_with(I::ArithmeticOverflow {}),
            CredisError::IndexOutOfBounds => revert_with(I::IndexOutOfBounds {}),
            CredisError::NonTransferable => revert_with(I::NonTransferable {}),
            CredisError::UnknownProfileSelector(value) => {
                revert_with(I::UnknownProfileSelector { value })
            }
        }
    }
}
