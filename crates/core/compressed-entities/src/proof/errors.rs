//! Typed failures and source-preserving conversions for point-read proofs.

use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PointReadRequestError {
    #[error("unknown compressed-entity domain ID {0}")]
    UnknownDomain(u16),
}

#[derive(Debug, Error)]
pub enum PointReadServiceError {
    #[error("point proof hashing backend failed: {0}")]
    CryptoBackend(#[source] crate::CommitmentError),
    #[error("compressed-entity proof service is not ready at genesis")]
    GenesisUnavailable,
    #[error("compressed-entity proof materialization is inconsistent: {0}")]
    Materialization(String),
    #[error("compressed-entity proof materialization is inconsistent: {0}")]
    MaterializationSource(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("compiled proof length {actual} is outside v1 bounds")]
    MalformedProofLength { actual: usize },
    #[error("point proof package is invalid: {0}")]
    InvalidPackage(&'static str),
}

impl PointReadServiceError {
    pub(crate) fn materialization(error: impl Into<Self>) -> Self {
        error.into()
    }
}

impl From<crate::smt::TreeError> for PointReadServiceError {
    fn from(error: crate::smt::TreeError) -> Self {
        match error {
            crate::smt::TreeError::Poseidon(error @ crate::CommitmentError::CryptoBackend(_)) => {
                Self::CryptoBackend(error)
            }
            error => Self::MaterializationSource(Box::new(error)),
        }
    }
}

impl From<crate::CollectionError> for PointReadServiceError {
    fn from(error: crate::CollectionError) -> Self {
        match error {
            crate::CollectionError::Hash(error @ crate::CommitmentError::CryptoBackend(_)) => {
                Self::CryptoBackend(error)
            }
            error => Self::MaterializationSource(Box::new(error)),
        }
    }
}

impl From<crate::persistence::PersistenceError> for PointReadServiceError {
    fn from(error: crate::persistence::PersistenceError) -> Self {
        match error {
            crate::persistence::PersistenceError::CryptoBackend(
                error @ crate::CommitmentError::CryptoBackend(_),
            ) => Self::CryptoBackend(error),
            error => Self::MaterializationSource(Box::new(error)),
        }
    }
}

impl From<crate::tree_manager::TreeServiceError> for PointReadServiceError {
    fn from(error: crate::tree_manager::TreeServiceError) -> Self {
        match error {
            crate::tree_manager::TreeServiceError::Persistence(error) => error.into(),
            error => Self::MaterializationSource(Box::new(error)),
        }
    }
}

impl From<crate::staging::StagingError> for PointReadServiceError {
    fn from(error: crate::staging::StagingError) -> Self {
        match error {
            crate::staging::StagingError::Hash(
                error @ crate::CommitmentError::CryptoBackend(_),
            ) => Self::CryptoBackend(error),
            crate::staging::StagingError::Collection(crate::CollectionError::Hash(
                error @ crate::CommitmentError::CryptoBackend(_),
            )) => Self::CryptoBackend(error),
            crate::staging::StagingError::Persistence(error) => error.into(),
            error => Self::MaterializationSource(Box::new(error)),
        }
    }
}
