use thiserror::Error;

/// Failure returned by the finalized-parent body/index adapter.
#[derive(Debug, Error)]
pub enum ParentBodySourceError {
    /// The local backend could not serve the request. This is not canonical
    /// absence and must enter the ADR-005 recovery path.
    #[error("parent body source unavailable: {0}")]
    Unavailable(String),
    /// The local projection violated a canonical body/index invariant.
    #[error("parent body source corruption: {0}")]
    Corruption(String),
}

impl From<ParentBodySourceError> for outbe_primitives::error::PrecompileError {
    fn from(value: ParentBodySourceError) -> Self {
        match value {
            ParentBodySourceError::Unavailable(message) => Self::BodyReadUnavailable(message),
            ParentBodySourceError::Corruption(message) => Self::BodyReadCorruption(message),
        }
    }
}
