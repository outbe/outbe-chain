use alloy_primitives::Bytes;

use crate::storage::SubCallError;

/// Precompile error types.
///
/// Marked `#[non_exhaustive]` so adding future variants is forward-compatible
/// for downstream crates that match on this enum.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrecompileError {
    /// Out of gas.
    #[error("out of gas")]
    OutOfGas,

    /// Storage operation failed.
    #[error("storage error: {0}")]
    Storage(String),

    /// The node-local off-chain body backend could not serve this execution read.
    #[error("body read unavailable: {0}")]
    BodyReadUnavailable(String),

    /// The local consensus request expired while waiting for an off-chain read.
    #[error("body read request deadline exceeded")]
    BodyReadRequestDeadline,

    /// A body or body index violated its deterministic repository invariants.
    #[error("body read corruption: {0}")]
    BodyReadCorruption(String),

    /// Exact finalized compressed-entity tree materialization is unavailable
    /// to this node. This is local readiness, not evidence of block invalidity.
    #[error("compressed-entity tree unavailable: {0}")]
    TreeUnavailable(String),

    /// One transaction can never fit inside the configured CE work budget.
    #[error("transaction exceeds the compressed-entity work limit")]
    TransactionCeWorkLimitExceeded,

    /// This payload has exhausted its deterministic CE work budget.
    #[error("block compressed-entity work capacity exhausted")]
    BlockCeWorkCapacityExhausted,

    /// Write attempted during static call.
    #[error("write protection: cannot modify state during static call")]
    WriteProtection,

    /// User-triggerable error - transaction reverts but does not halt the EVM.
    #[error("revert: {0}")]
    Revert(String),

    /// Sub-call revert that carries the raw returndata bytes from the child
    /// frame. Used by sub-call API to surface Solidity revert payloads to the
    /// caller.
    #[error("revert with bytes: {0}")]
    RevertBytes(Bytes),

    /// Sub-call halt forwarded from `run_sub_call_impl`.
    #[error("sub-call error: {0}")]
    SubCall(SubCallError),

    /// Operation is not supported by this provider (e.g., `set_code` on
    /// `DirectStorageProvider`).
    #[error("unsupported operation")]
    Unsupported,

    /// Fatal / unrecoverable error.
    #[error("fatal: {0}")]
    Fatal(String),
}

/// Result type alias for precompile operations.
pub type Result<T> = std::result::Result<T, PrecompileError>;

impl From<SubCallError> for PrecompileError {
    fn from(value: SubCallError) -> Self {
        PrecompileError::SubCall(value)
    }
}

/// What a system sweep does with one item's error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepFailure {
    /// The same on every node: roll the item back and move on.
    Skip,
    /// The sweep's gas budget is spent: end the pass and resume next block.
    Stop,
    /// Node-local, or already recorded against the transaction: fail the block.
    Propagate,
}

impl PrecompileError {
    /// A failure of this node's own storage or readers, which another node would not hit.
    pub fn is_node_local(&self) -> bool {
        matches!(
            self,
            Self::Storage(_)
                | Self::BodyReadUnavailable(_)
                | Self::BodyReadRequestDeadline
                | Self::TreeUnavailable(_)
        )
    }

    pub fn sweep_failure(&self) -> SweepFailure {
        match self {
            error if error.is_node_local() => SweepFailure::Propagate,
            Self::TransactionCeWorkLimitExceeded | Self::BlockCeWorkCapacityExhausted => {
                SweepFailure::Propagate
            }
            Self::OutOfGas => SweepFailure::Stop,
            _ => SweepFailure::Skip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PrecompileError, SweepFailure};
    use crate::storage::SubCallError;

    #[test]
    fn sweep_failures_follow_where_the_error_comes_from() {
        let table = [
            (PrecompileError::OutOfGas, false, SweepFailure::Stop),
            (
                PrecompileError::Storage("x".into()),
                true,
                SweepFailure::Propagate,
            ),
            (
                PrecompileError::BodyReadUnavailable("x".into()),
                true,
                SweepFailure::Propagate,
            ),
            (
                PrecompileError::BodyReadRequestDeadline,
                true,
                SweepFailure::Propagate,
            ),
            (
                PrecompileError::BodyReadCorruption("x".into()),
                false,
                SweepFailure::Skip,
            ),
            (
                PrecompileError::TreeUnavailable("x".into()),
                true,
                SweepFailure::Propagate,
            ),
            (
                PrecompileError::TransactionCeWorkLimitExceeded,
                false,
                SweepFailure::Propagate,
            ),
            (
                PrecompileError::BlockCeWorkCapacityExhausted,
                false,
                SweepFailure::Propagate,
            ),
            (PrecompileError::WriteProtection, false, SweepFailure::Skip),
            (
                PrecompileError::Revert("x".into()),
                false,
                SweepFailure::Skip,
            ),
            (
                PrecompileError::RevertBytes(Default::default()),
                false,
                SweepFailure::Skip,
            ),
            (
                PrecompileError::SubCall(SubCallError::OutOfGas),
                false,
                SweepFailure::Skip,
            ),
            (PrecompileError::Unsupported, false, SweepFailure::Skip),
            (
                PrecompileError::Fatal("x".into()),
                false,
                SweepFailure::Skip,
            ),
        ];
        for (error, node_local, failure) in table {
            assert_eq!(error.is_node_local(), node_local, "{error}");
            assert_eq!(error.sweep_failure(), failure, "{error}");
        }
    }
}
