use alloy_primitives::Bytes;
use alloy_sol_types::{Revert, SolError};
use revm::precompile::{PrecompileHalt, PrecompileOutput, PrecompileResult};

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

    /// Provider-level sub-call error, such as `NotAvailable` or database failure.
    #[error("sub-call error: {0}")]
    SubCall(SubCallError),
    /// Deterministic execution halt, propagated through revm's precompile result.
    #[error("execution halted: {0:?}")]
    Halt(PrecompileHalt),

    /// Operation is not supported by this provider.
    #[error("unsupported operation")]
    Unsupported,

    /// Fatal / unrecoverable error.
    #[error("fatal: {0}")]
    Fatal(String),

    /// This node's enclave could not serve the request. Another node's enclave would.
    #[error("enclave unavailable: {0}")]
    EnclaveUnavailable(String),
}

/// Result type alias for precompile operations.
pub type Result<T> = std::result::Result<T, PrecompileError>;

impl From<SubCallError> for PrecompileError {
    fn from(value: SubCallError) -> Self {
        match value {
            SubCallError::ParentOutOfGas | SubCallError::OutOfGas => Self::OutOfGas,
            SubCallError::StaticContextViolation | SubCallError::StateChangeDuringStaticCall => {
                Self::WriteProtection
            }
            SubCallError::EvmHalt(revm::context::result::HaltReason::OutOfGas(_)) => Self::OutOfGas,
            SubCallError::EvmHalt(
                revm::context::result::HaltReason::StateChangeDuringStaticCall
                | revm::context::result::HaltReason::CallNotAllowedInsideStatic,
            ) => Self::WriteProtection,
            error @ (SubCallError::DepthLimitExceeded
            | SubCallError::InvalidTarget
            | SubCallError::NotActivated
            | SubCallError::EvmHalt(_)) => Self::Halt(PrecompileHalt::other(error.to_string())),
            error @ (SubCallError::NotAvailable
            | SubCallError::ProviderBorrowed
            | SubCallError::DatabaseError(_)
            | SubCallError::Fatal(_)) => Self::SubCall(error),
        }
    }
}

impl PrecompileError {
    /// Convert domain errors at one boundary, following Tempo's precompile error model.
    /// Execution failures return Revert/Halt; infrastructure failures return Fatal.
    pub fn into_precompile_result(self, gas: u64) -> PrecompileResult {
        match self {
            Self::OutOfGas => Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, 0)),
            Self::WriteProtection => Ok(PrecompileOutput::halt(
                PrecompileHalt::other_static("state change during static call"),
                0,
            )),
            Self::Halt(reason) => Ok(PrecompileOutput::halt(reason, 0)),
            Self::Revert(message) => Ok(PrecompileOutput::revert(
                gas,
                Revert::from(message).abi_encode().into(),
                0,
            )),
            error @ Self::BodyReadCorruption(_) => Ok(PrecompileOutput::revert(
                gas,
                Revert::from(error.to_string()).abi_encode().into(),
                0,
            )),
            Self::RevertBytes(bytes) => Ok(PrecompileOutput::revert(gas, bytes, 0)),
            Self::SubCall(error) => Err(revm::precompile::PrecompileError::Fatal(format!(
                "sub-call error: {error:?}"
            ))),
            Self::Unsupported => Err(revm::precompile::PrecompileError::Fatal(
                "precompile reported Unsupported".into(),
            )),
            error @ (Self::Storage(_)
            | Self::BodyReadUnavailable(_)
            | Self::BodyReadRequestDeadline
            | Self::TreeUnavailable(_)
            | Self::TransactionCeWorkLimitExceeded
            | Self::BlockCeWorkCapacityExhausted
            | Self::Fatal(_)
            | Self::EnclaveUnavailable(_)) => {
                Err(revm::precompile::PrecompileError::Fatal(error.to_string()))
            }
        }
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
    /// A failure of this node's own storage, readers or enclave, which another node would not hit.
    pub fn is_node_local(&self) -> bool {
        match self {
            Self::Storage(_)
            | Self::BodyReadUnavailable(_)
            | Self::BodyReadRequestDeadline
            | Self::TreeUnavailable(_)
            | Self::EnclaveUnavailable(_)
            | Self::SubCall(SubCallError::DatabaseError(_)) => true,
            Self::OutOfGas
            | Self::BodyReadCorruption(_)
            | Self::TransactionCeWorkLimitExceeded
            | Self::BlockCeWorkCapacityExhausted
            | Self::WriteProtection
            | Self::Revert(_)
            | Self::RevertBytes(_)
            | Self::Halt(_)
            | Self::SubCall(_)
            | Self::Unsupported
            | Self::Fatal(_) => false,
        }
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

/// What deciding one sweep item came to.
pub enum Decided<T> {
    Done(T),
    /// A deterministic failure, rolled back with the item.
    Skipped(PrecompileError),
    /// The gas ran out before the item: the sweep resumes on it next block.
    Stopped,
}

/// Sorts a sweep item's error: a node-local one fails the block.
pub fn decide<T>(
    outcome: Result<T>,
    classify: impl Fn(&PrecompileError) -> SweepFailure,
) -> Result<Decided<T>> {
    match outcome {
        Ok(value) => Ok(Decided::Done(value)),
        Err(error) => match classify(&error) {
            SweepFailure::Propagate => Err(error),
            SweepFailure::Stop => Ok(Decided::Stopped),
            SweepFailure::Skip => Ok(Decided::Skipped(error)),
        },
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
            (
                PrecompileError::SubCall(SubCallError::DatabaseError("x".into())),
                true,
                SweepFailure::Propagate,
            ),
            (PrecompileError::Unsupported, false, SweepFailure::Skip),
            (
                PrecompileError::Fatal("x".into()),
                false,
                SweepFailure::Skip,
            ),
            (
                PrecompileError::EnclaveUnavailable("x".into()),
                true,
                SweepFailure::Propagate,
            ),
        ];
        for (error, node_local, failure) in table {
            assert_eq!(error.is_node_local(), node_local, "{error}");
            assert_eq!(error.sweep_failure(), failure, "{error}");
        }
    }
}
