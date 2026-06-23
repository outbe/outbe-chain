use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisFactoryError {
    #[error("fidelity index not eligible")]
    FidelityNotEligible,
}

impl From<GratisFactoryError> for PrecompileError {
    fn from(err: GratisFactoryError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
