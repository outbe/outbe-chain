use alloy_primitives::Bytes;
use alloy_sol_types::{Revert, SolError};
use revm::precompile::{PrecompileHalt, PrecompileOutput, PrecompileResult};

use crate::storage::SubCallError;

/// Precompile error types.
///
/// The `#[non_exhaustive]` attribute lets future variants remain compatible
/// with downstream crates that match on this enum.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrecompileError {
    /// Gas ran out.
    #[error("out of gas")]
    OutOfGas,

    /// Storage operation failed.
    #[error("storage error: {0}")]
    Storage(String),

    /// The local body backend could not serve this execution read.
    #[error("body read unavailable: {0}")]
    BodyReadUnavailable(String),

    /// The local consensus request expired while waiting for a body read.
    #[error("body read request deadline exceeded")]
    BodyReadRequestDeadline,

    /// A body or body index broke deterministic repository invariants.
    #[error("body read corruption: {0}")]
    BodyReadCorruption(String),

    /// The finalized compressed-entity tree is unavailable on this node.
    /// This state reflects local readiness, not an invalid block.
    #[error("compressed-entity tree unavailable: {0}")]
    TreeUnavailable(String),

    /// One transaction exceeds the configured compressed-entity work budget.
    #[error("transaction exceeds the compressed-entity work limit")]
    TransactionCeWorkLimitExceeded,

    /// This payload exhausted its deterministic compressed-entity work budget.
    #[error("block compressed-entity work capacity exhausted")]
    BlockCeWorkCapacityExhausted,

    /// The caller attempted a state write during a static call.
    #[error("write protection: cannot modify state during static call")]
    WriteProtection,

    /// A user error reverted the transaction without halting the EVM.
    #[error("revert: {0}")]
    Revert(String),

    /// A child frame reverted and returned raw error bytes.
    /// The sub-call API uses this variant to return Solidity revert payloads.
    #[error("revert with bytes: {0}")]
    RevertBytes(Bytes),

    /// A provider sub-call failed.
    #[error("sub-call error: {0}")]
    SubCall(SubCallError),
    /// Execution halted deterministically through the revm precompile result.
    #[error("execution halted: {0:?}")]
    Halt(PrecompileHalt),

    /// This provider does not support the operation.
    #[error("unsupported operation")]
    Unsupported,

    /// Unrecoverable fatal error.
    #[error("fatal: {0}")]
    Fatal(String),

    /// This node enclave could not serve the request.
    #[error("enclave unavailable: {0}")]
    EnclaveUnavailable(String),
}

/// Result type alias for precompile operations.
pub type Result<T> = std::result::Result<T, PrecompileError>;

/// Errors that initial TEE genesis policy construction returns.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TeeGenesisPolicyError {
    #[error("TEE genesis policy requires a non-zero genesis hash")]
    ZeroGenesisHash,
    #[error("DcapRequired requires devnet, testnet, or mainnet chain ID ({devnet}, {testnet}, or {mainnet})")]
    InvalidDcapChainId {
        devnet: u64,
        testnet: u64,
        mainnet: u64,
    },
    #[error("GramineDirectDev requires devnet or testnet chain ID ({devnet} or {testnet})")]
    InvalidGramineDirectDevChainId { devnet: u64, testnet: u64 },
    #[error("derive normative resource schedule: {0}")]
    DeriveNormativeResourceSchedule(String),
    #[error("DcapRequired measurements must be non-zero")]
    ZeroDcapMeasurement,
    #[error("DcapRequired minimum TCB evaluation data number must be non-zero")]
    ZeroMinimumTcbEvaluationDataNumber,
    #[error("encode initial TEE policy: {0}")]
    EncodeInitialPolicy(String),
    #[error("encode initial TEE policy schedule: {0}")]
    EncodeInitialPolicySchedule(String),
    #[error("hash initial TEE policy schedule: {0}")]
    HashInitialPolicySchedule(String),
}

impl TeeGenesisPolicyError {
    #[must_use]
    pub fn contains(&self, pat: &str) -> bool {
        self.to_string().contains(pat)
    }

    pub(crate) fn derive_normative_resource_schedule(error: impl std::fmt::Display) -> Self {
        Self::DeriveNormativeResourceSchedule(error.to_string())
    }

    pub(crate) fn encode_initial_policy(error: impl std::fmt::Display) -> Self {
        Self::EncodeInitialPolicy(error.to_string())
    }

    pub(crate) fn encode_initial_policy_schedule(error: impl std::fmt::Display) -> Self {
        Self::EncodeInitialPolicySchedule(error.to_string())
    }

    pub(crate) fn hash_initial_policy_schedule(error: impl std::fmt::Display) -> Self {
        Self::HashInitialPolicySchedule(error.to_string())
    }
}

impl From<TeeGenesisPolicyError> for String {
    fn from(error: TeeGenesisPolicyError) -> Self {
        error.to_string()
    }
}

/// Errors that [`crate::tee_operator_v1::TeeRenewalScheduleV1`] validation returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TeeRenewalScheduleError {
    #[error("renewal schedule contains a zero mandatory field")]
    ZeroMandatoryField,
    #[error("renewal schedule activation height overflow")]
    ActivationHeightOverflow,
    #[error("renewal schedule activation height is inconsistent")]
    InconsistentActivationHeight,
    #[error("renewal schedule freeze height is inconsistent")]
    InconsistentFreezeHeight,
}

impl TeeRenewalScheduleError {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ZeroMandatoryField => "renewal schedule contains a zero mandatory field",
            Self::ActivationHeightOverflow => "renewal schedule activation height overflow",
            Self::InconsistentActivationHeight => {
                "renewal schedule activation height is inconsistent"
            }
            Self::InconsistentFreezeHeight => "renewal schedule freeze height is inconsistent",
        }
    }

    #[must_use]
    pub fn to_owned(self) -> String {
        self.as_str().to_owned()
    }
}

impl std::ops::Deref for TeeRenewalScheduleError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl From<TeeRenewalScheduleError> for &'static str {
    fn from(error: TeeRenewalScheduleError) -> Self {
        error.as_str()
    }
}

impl From<TeeRenewalScheduleError> for String {
    fn from(error: TeeRenewalScheduleError) -> Self {
        error.as_str().to_owned()
    }
}

/// Errors that [`crate::time::WorldwideDay`] string parsing returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorldwideDayParseError {
    #[error("worldwide_day must be a valid u32 value")]
    InvalidU32,
    #[error("worldwide_day must be a valid YYYYMMDD date")]
    InvalidDate,
}

impl WorldwideDayParseError {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidU32 => "worldwide_day must be a valid u32 value",
            Self::InvalidDate => "worldwide_day must be a valid YYYYMMDD date",
        }
    }

    #[must_use]
    pub fn contains(self, pat: &str) -> bool {
        self.as_str().contains(pat)
    }
}

impl std::ops::Deref for WorldwideDayParseError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl From<WorldwideDayParseError> for String {
    fn from(error: WorldwideDayParseError) -> Self {
        error.as_str().to_owned()
    }
}

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
    /// Construct a [`PrecompileError::Revert`] from a message.
    pub fn revert(message: impl Into<String>) -> Self {
        Self::Revert(message.into())
    }

    /// Construct a [`PrecompileError::Fatal`] from a message.
    pub fn fatal(message: impl Into<String>) -> Self {
        Self::Fatal(message.into())
    }

    /// Construct a [`PrecompileError::Storage`] from a message.
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage(message.into())
    }

    /// Construct a [`PrecompileError::Storage`] from a displayable error.
    pub fn storage_err(error: impl std::fmt::Display) -> Self {
        Self::Storage(error.to_string())
    }

    /// Construct a [`PrecompileError::BodyReadUnavailable`] from a message.
    pub fn body_read_unavailable(message: impl Into<String>) -> Self {
        Self::BodyReadUnavailable(message.into())
    }

    /// Construct a [`PrecompileError::BodyReadCorruption`] from a message.
    pub fn body_read_corruption(message: impl Into<String>) -> Self {
        Self::BodyReadCorruption(message.into())
    }

    /// Convert domain errors into a precompile result.
    /// Execution failures return Revert or Halt, and infrastructure failures return Fatal.
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

/// Action that a system sweep takes for one item error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepFailure {
    /// Roll the item back on every node and continue the sweep.
    Skip,
    /// End the sweep when gas runs out and resume in the next block.
    Stop,
    /// Fail the block when the error is local to this node.
    Propagate,
}

impl PrecompileError {
    /// Return true when this error comes from local storage, readers, or enclave state.
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

/// Outcome for one sweep item.
pub enum Decided<T> {
    Done(T),
    /// Deterministic failure that rolled back the item.
    Skipped(PrecompileError),
    /// Gas ran out before the item finished, so the next block resumes it.
    Stopped,
}

/// Classify a sweep item error and propagate node-local failures.
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
