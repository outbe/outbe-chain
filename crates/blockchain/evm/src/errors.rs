//! Typed errors for Outbe EVM setup, dispatch, and blocks.

use alloy_primitives::{Address, B256, U256};
use core::fmt::Display;
use outbe_primitives::{
    error::PrecompileError, storage::MetadosisMutationEntitlements,
    storage::MetadosisMutationPurposeTag,
};
use std::path::Path;
use std::sync::Arc;

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

/// Errors that occur when the node checks TEE genesis and ChainSpec data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeeAttestationActivationError {
    #[error("seeded genesis path is not UTF-8")]
    SeededGenesisPathNotUtf8,
    #[error("parse seeded genesis ChainSpec: {0}")]
    ParseSeededGenesisChainSpec(String),
    #[error("read seeded genesis {path}: {reason}")]
    ReadSeededGenesis { path: String, reason: String },
    #[error("parse seeded genesis JSON: {0}")]
    ParseSeededGenesisJson(String),
    #[error("seeded genesis has no alloc object")]
    MissingSeededGenesisAlloc,
    #[error("seeded genesis has no ValidatorSet allocation")]
    MissingValidatorSetAllocation,
    #[error("seeded genesis ValidatorSet has no storage")]
    MissingValidatorSetStorage,
    #[error("seeded genesis validator count does not fit usize")]
    ValidatorCountOverflow,
    #[error("seeded genesis validator count is outside 1..=256")]
    ValidatorCountOutOfRange,
    #[error("seeded genesis contains an invalid validator address")]
    InvalidValidatorAddress,
    #[error("seeded genesis committee member is not ACTIVE with a BLS share")]
    CommitteeMemberNotActiveWithBlsShare,
    #[error("seeded genesis consensus key high word is not right-padded")]
    ConsensusKeyHighWordNotRightPadded,
    #[error("seeded genesis contains an invalid MinPk consensus key")]
    InvalidMinPkConsensusKey,
    #[error("seeded genesis contains duplicate consensus keys")]
    DuplicateConsensusKeys,
    #[error("seeded genesis lacks ValidatorSet {label}")]
    MissingValidatorSetWord { label: String },
    #[error("decode ValidatorSet {label}: {reason}")]
    DecodeValidatorSetWord { label: String, reason: String },
    #[error("seeded genesis ValidatorSet {label} exceeds one word")]
    ValidatorSetWordExceedsOneWord { label: String },
    #[error("invalid active TEE policy at block {height}: {reason}")]
    InvalidActiveTeePolicy { height: u64, reason: String },
    #[error("DCAP genesis path is not UTF-8")]
    DcapGenesisPathNotUtf8,
    #[error("parse DCAP genesis ChainSpec: {0}")]
    ParseDcapGenesisChainSpec(String),
    #[error("{0}")]
    InvalidChainSpecActivation(String),
    #[error("DCAP release ChainSpec requires DcapRequired policy")]
    RequiresDcapRequiredPolicy,
    #[error("encode DCAP TEE policy: {0}")]
    EncodeDcapTeePolicy(String),
    #[error("hash DCAP TEE policy: {0}")]
    HashDcapTeePolicy(String),
    #[error("encode DCAP TEE policy schedule: {0}")]
    EncodeDcapTeePolicySchedule(String),
    #[error(
        "active DCAP policy must admit Platform UpToDate | SWHardeningNeeded | ConfigurationAndSWHardeningNeeded and QE UpToDate"
    )]
    InvalidDcapTcbStatuses,
    #[error("active DCAP policy does not exactly bind the signed bundle measurement")]
    ReleaseMeasurementMismatch,
    #[error("derive canonical initial DCAP policy: {0}")]
    DeriveCanonicalInitialDcapPolicy(String),
    #[error(
        "active DCAP policy does not equal the canonical initial policy for the signed release"
    )]
    NonCanonicalInitialDcapPolicy,
    #[error("{field}.activationHeight must be {expected}")]
    InvalidActivationHeight { field: &'static str, expected: u64 },
    #[error("invalid canonical TEE policy schedule: {0}")]
    InvalidCanonicalPolicySchedule(String),
    #[error("cannot hash TEE policy schedule: {0}")]
    CannotHashPolicySchedule(String),
    #[error("TEE policy schedule hash does not match canonical bytes")]
    PolicyScheduleHashMismatch,
    #[error("cannot derive normative resource schedule: {0}")]
    CannotDeriveNormativeResourceSchedule(String),
    #[error("TEE manifest does not bind the normative resource schedule")]
    ManifestResourceScheduleMismatch,
    #[error("TEE schedule has no block-1 policy: {0}")]
    MissingBlock1Policy(String),
    #[error(
        "DcapRequired requires devnet, testnet, or mainnet chain ID ({devnet}, {testnet}, or {mainnet})"
    )]
    DcapRequiredChainIdDisallowed {
        devnet: u64,
        testnet: u64,
        mainnet: u64,
    },
    #[error("GramineDirectDev requires devnet or testnet chain ID ({devnet} or {testnet})")]
    GramineDirectDevChainIdDisallowed { devnet: u64, testnet: u64 },
    #[error("initial TEE policy does not bind the manifest resource schedule")]
    InitialPolicyResourceScheduleMismatch,
    #[error("TEE policy schedule does not match ChainSpec identity")]
    PolicyScheduleChainSpecIdentityMismatch,
    #[error("genesis TEE policy schedule must contain exactly one initial policy")]
    GenesisPolicyScheduleEntryCount,
    #[error("TEE policy schedule must be 0x-prefixed hexadecimal")]
    PolicyScheduleMissingHexPrefix,
    #[error("TEE policy schedule cap overflow")]
    PolicyScheduleCapOverflow,
    #[error("TEE policy schedule exceeds {max_bytes} bytes")]
    PolicyScheduleExceedsMaxBytes { max_bytes: usize },
    #[error("TEE policy schedule is not canonical hexadecimal bytes")]
    PolicyScheduleNotCanonicalHex,
    #[error("decode TEE policy schedule: {0}")]
    DecodePolicyScheduleHex(String),
    #[error("TEE attestation ChainSpec authority is not bound")]
    UnboundAuthority,
    #[error("genesis config is missing required {field}")]
    MissingGenesisConfigField { field: &'static str },
    #[error("invalid genesis config {field}: {reason}")]
    InvalidGenesisConfigField { field: &'static str, reason: String },
}

impl TeeAttestationActivationError {
    /// Checks whether the rendered error message contains the given text.
    #[must_use]
    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }

    pub(crate) fn parse_seeded_chain_spec(error: impl Display) -> Self {
        Self::ParseSeededGenesisChainSpec(error.to_string())
    }

    pub(crate) fn read_seeded_genesis(path: &Path, error: impl Display) -> Self {
        Self::ReadSeededGenesis {
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    }

    pub(crate) fn parse_seeded_json(error: impl Display) -> Self {
        Self::ParseSeededGenesisJson(error.to_string())
    }

    pub(crate) fn decode_validator_set_word(label: &str, error: impl Display) -> Self {
        Self::DecodeValidatorSetWord {
            label: label.to_owned(),
            reason: error.to_string(),
        }
    }

    pub(crate) fn invalid_active_policy(height: u64, error: impl Display) -> Self {
        Self::InvalidActiveTeePolicy {
            height,
            reason: error.to_string(),
        }
    }

    pub(crate) fn parse_dcap_chain_spec(error: impl Display) -> Self {
        Self::ParseDcapGenesisChainSpec(error.to_string())
    }

    pub(crate) fn invalid_chain_spec_activation(error: impl Display) -> Self {
        Self::InvalidChainSpecActivation(error.to_string())
    }

    pub(crate) fn invalid_genesis_config_field(field: &'static str, error: impl Display) -> Self {
        Self::InvalidGenesisConfigField {
            field,
            reason: error.to_string(),
        }
    }

    pub(crate) fn encode_dcap_policy(error: impl Display) -> Self {
        Self::EncodeDcapTeePolicy(error.to_string())
    }

    pub(crate) fn hash_dcap_policy(error: impl Display) -> Self {
        Self::HashDcapTeePolicy(error.to_string())
    }

    pub(crate) fn encode_dcap_policy_schedule(error: impl Display) -> Self {
        Self::EncodeDcapTeePolicySchedule(error.to_string())
    }

    pub(crate) fn derive_canonical_initial_dcap_policy(error: impl Display) -> Self {
        Self::DeriveCanonicalInitialDcapPolicy(error.to_string())
    }

    pub(crate) fn invalid_canonical_policy_schedule(error: impl Display) -> Self {
        Self::InvalidCanonicalPolicySchedule(error.to_string())
    }

    pub(crate) fn cannot_hash_policy_schedule(error: impl Display) -> Self {
        Self::CannotHashPolicySchedule(error.to_string())
    }

    pub(crate) fn cannot_derive_normative_resource_schedule(error: impl Display) -> Self {
        Self::CannotDeriveNormativeResourceSchedule(error.to_string())
    }

    pub(crate) fn missing_block1_policy(error: impl Display) -> Self {
        Self::MissingBlock1Policy(error.to_string())
    }

    pub(crate) fn decode_policy_schedule_hex(error: impl Display) -> Self {
        Self::DecodePolicyScheduleHex(error.to_string())
    }
}

impl From<TeeAttestationActivationError> for Arc<str> {
    fn from(error: TeeAttestationActivationError) -> Self {
        Arc::from(error.to_string())
    }
}

impl From<TeeAttestationActivationError> for String {
    fn from(error: TeeAttestationActivationError) -> Self {
        error.to_string()
    }
}

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

impl BeginBlockPrecompileError {
    pub(crate) fn invalid_tee_chain_spec_authority(error: impl Display) -> PrecompileError {
        Self::InvalidTeeChainSpecAuthority(error.to_string()).into()
    }

    pub(crate) fn invalid_active_tee_policy(error: impl Display) -> PrecompileError {
        Self::InvalidActiveTeePolicy(error.to_string()).into()
    }
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
        "Metadosis mutation purpose is not entitled for this EVM call: purpose={purpose:?} binding={binding:#x} remaining={remaining:?}"
    )]
    MetadosisMutationPurposeNotEntitled {
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        remaining: Box<MetadosisMutationEntitlements>,
    },
    #[error("Metadosis mutation frame is not active")]
    MetadosisMutationFrameNotActive,
    #[error("Metadosis mutation frame identity mismatch")]
    MetadosisMutationFrameIdentityMismatch,
    #[error("Metadosis mutation frame execution scope mismatch")]
    MetadosisMutationFrameScopeMismatch,
    #[error("certified Lysis activation frame is not entitled for this EVM call")]
    LysisActivationFrameNotEntitled,
    #[error("certified Lysis activation frame identity mismatch")]
    LysisActivationFrameIdentityMismatch,
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
            | CtxStorageError::MetadosisMutationPurposeNotEntitled { .. }
            | CtxStorageError::MetadosisMutationFrameNotActive
            | CtxStorageError::MetadosisMutationFrameIdentityMismatch
            | CtxStorageError::MetadosisMutationFrameScopeMismatch
            | CtxStorageError::LysisActivationFrameNotEntitled
            | CtxStorageError::LysisActivationFrameIdentityMismatch => {
                Self::Revert(error.to_string())
            }
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
