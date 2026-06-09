use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodError {
    #[error("invalid nodId length")]
    InvalidNodIdLength,

    #[error("invalid nodId hex")]
    InvalidNodIdHex,

    #[error("nod not found")]
    NodNotFound,

    #[error("bucket not found")]
    BucketNotFound,

    #[error("index out of bounds")]
    IndexOutOfBounds,
}

impl From<NodError> for PrecompileError {
    fn from(value: NodError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
