//! Error types for the PromisFactory module.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Errors that the PromisFactory runtime returns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromisFactoryError {
    #[error("native COEN amount overflow")]
    NativeCoenOverflow,
}

impl From<PromisFactoryError> for PrecompileError {
    fn from(err: PromisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
