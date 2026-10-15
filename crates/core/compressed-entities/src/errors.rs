//! Typed error definitions for the compressed-entities crate.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};
use thiserror::Error;

/// Errors that the finalized parent adapter returns.
#[derive(Debug, Error)]
pub enum ParentBodySourceError {
    #[error("parent body hashing backend unavailable: {0}")]
    CryptoBackendUnavailable(#[source] crate::CommitmentError),
    /// The consensus request expired while the node waited for the body.
    /// This error shows that the caller cancelled the request.
    #[error("parent body source request deadline exceeded: {0}")]
    RequestDeadline(String),
    /// The local backend did not serve the request.
    /// This error starts the ADR-005 recovery path.
    #[error("parent body source unavailable: {0}")]
    Unavailable(String),
    /// The local projection broke a canonical body or index rule.
    #[error("parent body source corruption: {0}")]
    Corruption(String),
}

impl From<ParentBodySourceError> for PrecompileError {
    fn from(value: ParentBodySourceError) -> Self {
        match value {
            ParentBodySourceError::CryptoBackendUnavailable(message) => {
                Self::CryptoBackendUnavailable(message.to_string())
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
    error.into()
}

pub(crate) fn collection_error(
    error: crate::CollectionError,
) -> outbe_primitives::error::PrecompileError {
    error.into()
}

pub(crate) fn hash_error(
    error: impl Into<PrecompileError>,
) -> outbe_primitives::error::PrecompileError {
    error.into()
}

impl From<crate::CommitmentError> for PrecompileError {
    fn from(error: crate::CommitmentError) -> Self {
        match error {
            error @ crate::CommitmentError::CryptoBackend(_) => {
                Self::CryptoBackendUnavailable(error.to_string())
            }
            error => fatal_error(error),
        }
    }
}

impl From<crate::CollectionError> for PrecompileError {
    fn from(error: crate::CollectionError) -> Self {
        match error {
            crate::CollectionError::Hash(error @ crate::CommitmentError::CryptoBackend(_)) => {
                error.into()
            }
            error => fatal_error(error),
        }
    }
}

impl From<crate::smt::TreeError> for PrecompileError {
    fn from(error: crate::smt::TreeError) -> Self {
        match error {
            crate::smt::TreeError::Poseidon(error @ crate::CommitmentError::CryptoBackend(_)) => {
                error.into()
            }
            error => fatal_error(error),
        }
    }
}

impl From<crate::persistence::PersistenceError> for PrecompileError {
    fn from(error: crate::persistence::PersistenceError) -> Self {
        match error {
            crate::persistence::PersistenceError::CryptoBackend(
                error @ crate::CommitmentError::CryptoBackend(_),
            ) => error.into(),
            error => fatal_error(error),
        }
    }
}

impl From<crate::staging::StagingError> for PrecompileError {
    fn from(error: crate::staging::StagingError) -> Self {
        match error {
            crate::staging::StagingError::Hash(error) => error.into(),
            crate::staging::StagingError::Collection(error) => error.into(),
            crate::staging::StagingError::Persistence(error) => error.into(),
            error => fatal_error(error),
        }
    }
}

impl From<crate::sharding::ShardingError> for PrecompileError {
    fn from(error: crate::sharding::ShardingError) -> Self {
        match error {
            crate::sharding::ShardingError::CryptoBackend(error) => error.into(),
            error => fatal_error(error),
        }
    }
}

/// Errors that the benchmark helpers return.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BenchSupportError {
    /// A benchmark helper failed.
    #[error("{0}")]
    Message(String),
    /// The record count does not fit in a 64-bit integer.
    #[error("benchmark record count is not representable as u64")]
    RecordCountOverflow,
    /// The search counter for benchmark keys overflowed.
    #[error("benchmark key search overflowed")]
    KeySearchOverflow,
}

impl BenchSupportError {
    /// Converts a displayable error into a benchmark error.
    pub(crate) fn message(error: impl core::fmt::Display) -> Self {
        Self::Message(error.to_string())
    }
}

/// Converts a displayable message into a fatal precompile error.
pub(crate) fn fatal_error(message: impl core::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(message.to_string())
}

/// Converts a displayable message into a revert precompile error.
pub(crate) fn revert_error(message: impl core::fmt::Display) -> PrecompileError {
    PrecompileError::Revert(message.to_string())
}

/// Converts a displayable message into a precompile error for body corruption.
pub(crate) fn body_read_corruption(message: impl core::fmt::Display) -> PrecompileError {
    PrecompileError::BodyReadCorruption(message.to_string())
}

/// Converts a displayable message into a fatal error for tree corruption.
pub(crate) fn tree_corruption(message: impl core::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(format!("compressed-entity tree corruption: {message}"))
}

/// Appends diagnostic parent data to a precompile error for body corruption.
pub(crate) fn enrich_body_read_corruption(
    error: PrecompileError,
    evm_block: Result<u64>,
    parent_root: B256,
    scope_parent_root: Result<B256>,
    diagnostic_binding: &str,
) -> PrecompileError {
    match error {
        PrecompileError::BodyReadCorruption(message) => PrecompileError::BodyReadCorruption(
            format!(
                "{message}; [CE_BODY_DIAGNOSTIC] evm_block={evm_block:?} evm_ce_root={parent_root} parent_root={scope_parent_root:?} {diagnostic_binding} thread={:?}",
                std::thread::current().id(),
            ),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;
    use crate::{
        persistence::PersistenceError, staging::StagingError, CollectionError, CommitmentError,
    };

    fn backend() -> CommitmentError {
        outbe_poseidon::PoseidonError::ResponseLength {
            index: 0,
            actual: 31,
        }
        .into()
    }

    #[test]
    fn typed_hash_wrappers_preserve_abort_and_invalid_input_semantics() {
        let failures = [
            hash_error(backend()),
            hash_error(CollectionError::Hash(backend())),
            hash_error(crate::smt::TreeError::Poseidon(backend())),
            hash_error(PersistenceError::CryptoBackend(backend())),
            hash_error(StagingError::Hash(backend())),
            hash_error(StagingError::Collection(CollectionError::Hash(backend()))),
            hash_error(StagingError::Persistence(PersistenceError::CryptoBackend(
                backend(),
            ))),
            hash_error(crate::ShardingError::CryptoBackend(backend())),
        ];
        for error in failures {
            assert!(
                matches!(error, PrecompileError::CryptoBackendUnavailable(_)),
                "{error:?}"
            );
        }

        let invalid = CommitmentError::from(outbe_poseidon::PoseidonError::InputLength);
        assert_eq!(invalid, CommitmentError::InputTooLong);
        for error in [
            hash_error(invalid.clone()),
            hash_error(CollectionError::Hash(invalid.clone())),
            hash_error(crate::smt::TreeError::Poseidon(invalid.clone())),
            hash_error(PersistenceError::CryptoBackend(invalid.clone())),
            hash_error(StagingError::Hash(invalid)),
            hash_error(crate::smt::TreeError::TreeHashError),
        ] {
            assert!(matches!(error, PrecompileError::Fatal(_)), "{error:?}");
        }
    }

    #[test]
    fn parent_and_materialization_errors_retain_typed_sources() {
        let parent = ParentBodySourceError::CryptoBackendUnavailable(backend());
        assert!(parent
            .source()
            .unwrap()
            .downcast_ref::<CommitmentError>()
            .is_some());
        assert!(matches!(
            PrecompileError::from(parent),
            PrecompileError::CryptoBackendUnavailable(_)
        ));

        let proof = crate::PointReadServiceError::materialization(StagingError::Persistence(
            PersistenceError::CryptoBackend(backend()),
        ));
        assert!(matches!(
            proof,
            crate::PointReadServiceError::CryptoBackend(_)
        ));
        assert!(proof
            .source()
            .unwrap()
            .downcast_ref::<CommitmentError>()
            .is_some());

        let proof =
            crate::PointReadServiceError::materialization(crate::smt::TreeError::TreeHashError);
        assert!(matches!(
            proof,
            crate::PointReadServiceError::MaterializationSource(_)
        ));
        assert_eq!(
            proof
                .source()
                .unwrap()
                .downcast_ref::<crate::smt::TreeError>(),
            Some(&crate::smt::TreeError::TreeHashError)
        );

        let persistence = PersistenceError::from(StagingError::InvalidShardEnvelope("bad root"));
        assert!(persistence
            .source()
            .unwrap()
            .downcast_ref::<Box<StagingError>>()
            .is_some());
    }
}
