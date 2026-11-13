//! Typed errors for Outbe EVM setup, dispatch, and blocks.

use alloy_primitives::{Address, B256, U256};
use core::fmt::Display;
use outbe_primitives::{
    error::PrecompileError,
    storage::{MutationEntitlements, MutationPurposeTag},
};

/// Checks whether a parent subcall out-of-gas overrides a nested precompile result.
#[must_use]
pub(crate) const fn is_overridden_by_subcall_out_of_gas(error: &PrecompileError) -> bool {
    matches!(
        error,
        PrecompileError::OutOfGas
            | PrecompileError::Revert(_)
            | PrecompileError::RevertBytes(_)
            | PrecompileError::Halt(_)
            | PrecompileError::WriteProtection
            | PrecompileError::BodyReadCorruption(_)
    )
}

pub use outbe_tee::errors::TeeAttestationActivationError;

/// Errors that occur when the config validates or recovers signers from system transactions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SystemTxExpectationError {
    #[error("decode Outbe block artifacts for system tx validation: {0}")]
    DecodeBlockArtifacts(String),
    #[error("{0}")]
    Layout(String),
    #[error("recover system tx signer: {0}")]
    RecoverSigner(String),
}

impl SystemTxExpectationError {
    pub(crate) fn decode_block_artifacts(error: impl Display) -> Self {
        Self::DecodeBlockArtifacts(error.to_string())
    }

    pub(crate) fn layout(error: impl Display) -> Self {
        Self::Layout(error.to_string())
    }

    pub(crate) fn recover_signer(error: impl Display) -> Self {
        Self::RecoverSigner(error.to_string())
    }
}

/// Failures of the block-1 TEE bootstrap check that the runtime host owns.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeeBootstrapHostError {
    /// Canonical preflight rejected the payload.
    #[error("invalid block-1 OST3 bootstrap payload: {0}")]
    InvalidPayload(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    /// The payload policy is not the policy resolved for this block.
    #[error("block-1 OST3 policy does not match the immutable ChainSpec schedule")]
    PolicyScheduleMismatch,
    /// No runtime host is installed, so the bootstrap check cannot run.
    #[error("evm runtime host is not installed")]
    HostNotInstalled,
}

/// Errors that occur in the begin-block system precompile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BeginBlockPrecompileError {
    #[error("system precompile can only be called by SYSTEM_ADDRESS")]
    InvalidSystemCaller,
    #[error("system precompile does not accept native token value")]
    NativeValueRejected,
    #[error("invalid system tx input: {0}")]
    InvalidSystemTxInput(String),
    #[error("LateFinalizeCredits: fb_number {fb_number} >= block {block_number}")]
    LateCreditFbNumberAtOrAboveBlock { fb_number: u64, block_number: u64 },
    #[error(
        "LateFinalizeCredits: fb_number {fb_number} outside inclusion window (distance {distance}, K={window_k}) for block {block_number}"
    )]
    LateCreditOutsideInclusionWindow {
        fb_number: u64,
        distance: u64,
        window_k: u64,
        block_number: u64,
    },
    #[error(
        "LateFinalizeCredits: inclusion distance {distance} exceeds u8 for block {block_number}"
    )]
    LateCreditInclusionDistanceExceedsU8 { distance: u64, block_number: u64 },
    #[error("invalid TEE ChainSpec authority: {0}")]
    InvalidTeeChainSpecAuthority(String),
    #[error("OST3 is not active at the current block")]
    Ost3NotActive,
    #[error("invalid active TEE policy: {0}")]
    InvalidActiveTeePolicy(String),
    #[error("OST3 policy does not match the active ChainSpec schedule")]
    Ost3PolicyScheduleMismatch,
}

impl From<BeginBlockPrecompileError> for PrecompileError {
    fn from(error: BeginBlockPrecompileError) -> Self {
        match error {
            BeginBlockPrecompileError::InvalidTeeChainSpecAuthority(_)
            | BeginBlockPrecompileError::InvalidActiveTeePolicy(_)
            | BeginBlockPrecompileError::InvalidSystemCaller
            | BeginBlockPrecompileError::NativeValueRejected
            | BeginBlockPrecompileError::InvalidSystemTxInput(_)
            | BeginBlockPrecompileError::LateCreditFbNumberAtOrAboveBlock { .. }
            | BeginBlockPrecompileError::LateCreditOutsideInclusionWindow { .. }
            | BeginBlockPrecompileError::LateCreditInclusionDistanceExceedsU8 { .. }
            | BeginBlockPrecompileError::Ost3NotActive
            | BeginBlockPrecompileError::Ost3PolicyScheduleMismatch => {
                Self::Revert(error.to_string())
            }
        }
    }
}

/// Errors that occur when the dispatcher checks a call frame or value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutbePrecompileDispatchError {
    #[error("outbe precompile: delegated call frame cannot execute a precompile")]
    DelegatedCallFrame,
    #[error("{0}")]
    RejectedBoundaryValue(&'static str),
    #[error("outbe precompile reentrancy denied")]
    ReentrancyDenied,
}

impl From<OutbePrecompileDispatchError> for PrecompileError {
    fn from(error: OutbePrecompileDispatchError) -> Self {
        Self::Revert(error.to_string())
    }
}

/// Fatal error returned when an Outbe precompile aborts the REVM interpreter.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutbePrecompileFatalError {
    #[error("{0}")]
    RevmPrecompile(String),
}

impl OutbePrecompileFatalError {
    pub(crate) fn from_revm(error: impl Display) -> Self {
        Self::RevmPrecompile(error.to_string())
    }
}

impl From<OutbePrecompileFatalError> for String {
    fn from(error: OutbePrecompileFatalError) -> Self {
        error.to_string()
    }
}

/// Errors that occur in the context storage provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[allow(clippy::large_enum_variant)]
pub enum CtxStorageError {
    #[error(
        "scoped mutation purpose is not entitled for this EVM call: purpose={purpose:?} binding={binding:#x} remaining={remaining:?}"
    )]
    ScopedMutationPurposeNotEntitled {
        purpose: MutationPurposeTag,
        binding: B256,
        remaining: Box<MutationEntitlements>,
    },
    #[error("scoped mutation frame is not active")]
    ScopedMutationFrameNotActive,
    #[error("scoped mutation frame identity mismatch")]
    ScopedMutationFrameIdentityMismatch,
    #[error("scoped mutation frame execution scope mismatch")]
    ScopedMutationFrameScopeMismatch,
    #[error("insufficient balance for transfer from {from}: needs {amount}")]
    InsufficientBalanceForTransfer { from: Address, amount: U256 },
    #[error("insufficient balance for burn from {address}: has {balance} but needs {amount}")]
    InsufficientBalanceForBurn {
        address: Address,
        balance: U256,
        amount: U256,
    },
    #[error("{0}")]
    StorageOperation(String),
    #[error("block_hash({number}) failed: {reason}")]
    BlockHashFailed { number: u64, reason: String },
    #[error("transfer failed: {0}")]
    TransferFailed(String),
    #[error("balance_incr failed: {0}")]
    BalanceIncrFailed(String),
    #[error("burn account load failed: {0}")]
    BurnAccountLoadFailed(String),
    #[error("burn set_balance failed: {0}")]
    BurnSetBalanceFailed(String),
}

impl CtxStorageError {
    pub(crate) fn storage(error: impl Display) -> PrecompileError {
        Self::StorageOperation(error.to_string()).into()
    }

    pub(crate) fn block_hash_failed(number: u64, error: impl Display) -> PrecompileError {
        Self::BlockHashFailed {
            number,
            reason: error.to_string(),
        }
        .into()
    }

    pub(crate) fn transfer_failed(error: impl Display) -> PrecompileError {
        Self::TransferFailed(error.to_string()).into()
    }

    pub(crate) fn balance_incr_failed(error: impl Display) -> PrecompileError {
        Self::BalanceIncrFailed(error.to_string()).into()
    }

    pub(crate) fn burn_account_load_failed(error: impl Display) -> PrecompileError {
        Self::BurnAccountLoadFailed(error.to_string()).into()
    }

    pub(crate) fn burn_set_balance_failed(error: impl Display) -> PrecompileError {
        Self::BurnSetBalanceFailed(error.to_string()).into()
    }
}

impl From<CtxStorageError> for PrecompileError {
    fn from(error: CtxStorageError) -> Self {
        match error {
            CtxStorageError::InsufficientBalanceForTransfer { .. }
            | CtxStorageError::InsufficientBalanceForBurn { .. }
            | CtxStorageError::ScopedMutationPurposeNotEntitled { .. }
            | CtxStorageError::ScopedMutationFrameNotActive
            | CtxStorageError::ScopedMutationFrameIdentityMismatch
            | CtxStorageError::ScopedMutationFrameScopeMismatch => Self::Revert(error.to_string()),
            CtxStorageError::StorageOperation(_)
            | CtxStorageError::BlockHashFailed { .. }
            | CtxStorageError::TransferFailed(_)
            | CtxStorageError::BalanceIncrFailed(_)
            | CtxStorageError::BurnAccountLoadFailed(_)
            | CtxStorageError::BurnSetBalanceFailed(_) => Self::Storage(error.to_string()),
        }
    }
}

/// Errors that occur while driving a nested EVM sub-call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubCallDriverError {
    #[error("{0}")]
    DatabaseError(String),
    #[error("unsupported subcall state-gas charge")]
    UnsupportedStateGasCharge,
    #[error("frame_init: {0}")]
    FrameInit(String),
    #[error("frame_run: {0}")]
    FrameRun(String),
    #[error("frame_return_result: {0}")]
    FrameReturnResult(String),
    #[error("frame_init nested: {0}")]
    FrameInitNested(String),
    #[error("non-VM child terminal: {0}")]
    NonVmChildTerminal(String),
    #[error("unexpected child terminal: {0}")]
    UnexpectedChildTerminal(String),
}

impl SubCallDriverError {
    pub(crate) fn database_error(
        error: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::DatabaseError(format!("{error:?}")).into()
    }

    pub(crate) fn frame_init(
        error: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::FrameInit(format!("{error:?}")).into()
    }

    pub(crate) fn frame_run(
        error: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::FrameRun(format!("{error:?}")).into()
    }

    pub(crate) fn frame_return_result(
        error: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::FrameReturnResult(format!("{error:?}")).into()
    }

    pub(crate) fn frame_init_nested(
        error: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::FrameInitNested(format!("{error:?}")).into()
    }

    pub(crate) fn non_vm_child_terminal(
        instr: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::NonVmChildTerminal(format!("{instr:?}")).into()
    }

    pub(crate) fn unexpected_child_terminal(
        other: impl core::fmt::Debug,
    ) -> outbe_primitives::storage::SubCallError {
        Self::UnexpectedChildTerminal(format!("{other:?}")).into()
    }
}

impl From<SubCallDriverError> for outbe_primitives::storage::SubCallError {
    fn from(error: SubCallDriverError) -> Self {
        match error {
            SubCallDriverError::DatabaseError(_) => Self::DatabaseError(error.to_string()),
            SubCallDriverError::UnsupportedStateGasCharge
            | SubCallDriverError::FrameInit(_)
            | SubCallDriverError::FrameRun(_)
            | SubCallDriverError::FrameReturnResult(_)
            | SubCallDriverError::FrameInitNested(_)
            | SubCallDriverError::NonVmChildTerminal(_)
            | SubCallDriverError::UnexpectedChildTerminal(_) => Self::Fatal(error.to_string()),
        }
    }
}

#[path = "errors/executor.rs"]
mod executor;
pub use executor::{
    BurnedFeeSettlementError, ExecutorStorageError, ExecutorValidationError, SystemTxBuildError,
};
