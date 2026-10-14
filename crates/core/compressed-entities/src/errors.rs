use thiserror::Error;

pub(crate) fn infrastructure_cause(
    mut error: &(dyn std::error::Error + 'static),
) -> Option<crate::CommitmentError> {
    loop {
        if let Some(cause) = error.downcast_ref::<crate::CommitmentError>() {
            if cause.is_infrastructure() {
                return Some(cause.clone());
            }
        }
        error = error.source()?;
    }
}

/// Failure returned by the finalized-parent body/index adapter.
#[derive(Debug, Error)]
pub enum ParentBodySourceError {
    #[error("parent body hashing backend unavailable: {0}")]
    CryptoBackendUnavailable(String),
    /// The node-local consensus request expired while waiting for the body.
    /// This is request cancellation, not backend unavailability.
    #[error("parent body source request deadline exceeded: {0}")]
    RequestDeadline(String),
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
            ParentBodySourceError::CryptoBackendUnavailable(message) => {
                Self::CryptoBackendUnavailable(message)
            }
            ParentBodySourceError::RequestDeadline(_) => Self::BodyReadRequestDeadline,
            ParentBodySourceError::Unavailable(message) => Self::BodyReadUnavailable(message),
            ParentBodySourceError::Corruption(message) => Self::BodyReadCorruption(message),
        }
    }
}

pub(crate) fn commitment_error(
    error: crate::CommitmentError,
) -> outbe_primitives::error::PrecompileError {
    if error.is_infrastructure() {
        outbe_primitives::error::PrecompileError::CryptoBackendUnavailable(error.to_string())
    } else {
        outbe_primitives::error::PrecompileError::Fatal(error.to_string())
    }
}

pub(crate) fn collection_error(
    error: crate::CollectionError,
) -> outbe_primitives::error::PrecompileError {
    if error.is_infrastructure() {
        outbe_primitives::error::PrecompileError::CryptoBackendUnavailable(error.to_string())
    } else {
        outbe_primitives::error::PrecompileError::Fatal(error.to_string())
    }
}

pub(crate) fn hash_error(
    error: impl std::error::Error + 'static,
) -> outbe_primitives::error::PrecompileError {
    match infrastructure_cause(&error) {
        Some(cause) => commitment_error(cause),
        None => outbe_primitives::error::PrecompileError::Fatal(error.to_string()),
    }
}
