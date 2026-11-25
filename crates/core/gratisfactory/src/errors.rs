//! Error types for the GratisFactory precompile.

use crate::precompile::IGratisFactory;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the GratisFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisFactoryError {
    #[error("native COEN amount overflow")]
    NativeCoenOverflow,
    #[error("COEN amount is zero or not a whole number of Gratis minors")]
    InvalidCoenAmount,
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<GratisFactoryError> for PrecompileError {
    fn from(err: GratisFactoryError) -> Self {
        use IGratisFactory as I;
        match err {
            GratisFactoryError::NativeCoenOverflow => revert_with(I::NativeCoenOverflow {}),
            GratisFactoryError::InvalidCoenAmount => revert_with(I::InvalidCoenAmount {}),
        }
    }
}
