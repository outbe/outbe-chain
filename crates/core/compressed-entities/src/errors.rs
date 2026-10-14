//! Typed error definitions for the compressed-entities crate.

use alloy_primitives::B256;
use outbe_primitives::error::{PrecompileError, Result};
use thiserror::Error;

/// Errors that the finalized parent adapter returns.
#[derive(Debug, Error)]
pub enum ParentBodySourceError {
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
            ParentBodySourceError::RequestDeadline(_) => Self::BodyReadRequestDeadline,
            ParentBodySourceError::Unavailable(message) => Self::BodyReadUnavailable(message),
            ParentBodySourceError::Corruption(message) => Self::BodyReadCorruption(message),
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
