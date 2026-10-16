use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{Revert, SolError};
use revm::precompile::{PrecompileHalt, PrecompileOutput, PrecompileResult};

use crate::stablecoin::CurrencyCodeError;
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

/// Errors returned by [`crate::consensus_metadata`] wire and RLP codecs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConsensusMetadataError {
    #[error("invalid ParentParticipationProof tag: {tag}")]
    InvalidParentParticipationProofTag { tag: u8 },
    #[error("certified-parent accounting metadata too short")]
    MetadataTooShort,
    #[error("invalid certified-parent accounting magic")]
    InvalidMagic,
    #[error("unsupported certified-parent accounting version: {version}")]
    UnsupportedVersion { version: u8 },
    #[error("trailing bytes in certified-parent accounting metadata")]
    TrailingBytes,
    #[error("invalid certified-parent accounting metadata RLP: {reason}")]
    InvalidRlp { reason: String },
    #[error("trailing bytes in certified-parent accounting metadata RLP")]
    TrailingBytesRlp,
    #[error("unexpected EOF reading u8")]
    UnexpectedEofU8,
    #[error("unexpected EOF reading missed-proposer count")]
    UnexpectedEofMissedProposerCount,
    #[error("invalid missed-proposer count slice")]
    InvalidMissedProposerCountSlice,
    #[error("unexpected EOF reading missed-proposer validator")]
    UnexpectedEofMissedProposerValidator,
    #[error("{name} list exceeds u16 count limit: {count}")]
    ListExceedsU16Limit { name: &'static str, count: usize },
    #[error("{name} exceeds u32 length limit: {len}")]
    PayloadExceedsU32Limit { name: &'static str, len: usize },
    #[error("unexpected EOF reading u64")]
    UnexpectedEofU64,
    #[error("invalid u64 slice length")]
    InvalidU64SliceLength,
    #[error("unexpected EOF reading B256")]
    UnexpectedEofB256,
    #[error("unexpected EOF reading address count")]
    UnexpectedEofAddressCount,
    #[error("invalid address count slice length")]
    InvalidAddressCountSliceLength,
    #[error("address list length overflow")]
    AddressListLengthOverflow,
    #[error("unexpected EOF reading address list")]
    UnexpectedEofAddressList,
    #[error("unexpected EOF reading byte length")]
    UnexpectedEofByteLength,
    #[error("invalid byte length slice")]
    InvalidByteLengthSlice,
    #[error("unexpected EOF reading bytes")]
    UnexpectedEofBytes,
}

impl From<ConsensusMetadataError> for PrecompileError {
    fn from(err: ConsensusMetadataError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::reshare_artifact`] envelope and record codecs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReshareArtifactError {
    #[error("repository publication requires OART v0x0C")]
    PublicationRequiresV0c,
    #[error("duplicate repository publication batch")]
    DuplicateRadiclePublication,
    #[error("too many block artifact records")]
    TooManyRecords,
    #[error("block artifact length overflow")]
    LengthOverflow,
    #[error("block artifacts exceed extra_data budget: {total_len} > {max_len}")]
    ExceedsExtraDataBudget { total_len: usize, max_len: usize },
    #[error("truncated block artifact record header")]
    TruncatedRecordHeader,
    #[error("block artifact record overflow")]
    RecordOverflow,
    #[error("truncated block artifact record payload")]
    TruncatedRecordPayload,
    #[error("trailing bytes in block artifacts")]
    TrailingBytes,
    #[error(
        "pre-final block artifacts exceed reserved extra_data budget: {encoded_len} > {max_len}"
    )]
    PrefinalExceedsReservedBudget { encoded_len: usize, max_len: usize },
    #[error("TEE expiry exclusions exceed protocol cap: {count} > {max_count}")]
    TeeExpiryExclusionsExceedCap { count: usize, max_count: usize },
    #[error("TEE expiry exclusions contain zero validator address")]
    TeeExpiryExclusionsZeroValidator,
    #[error("TEE expiry exclusions contain duplicate validator address")]
    TeeExpiryExclusionsDuplicateValidator,
    #[error("{name} list exceeds u16 count limit: {count}")]
    ListExceedsU16Limit { name: &'static str, count: usize },
    #[error("{name} exceeds u32 length limit: {len}")]
    ExceedsU32LengthLimit { name: &'static str, len: usize },
    #[error("{name} payload exceeds u16 length limit: {len}")]
    PayloadExceedsU16LengthLimit { name: &'static str, len: usize },
    #[error("block artifacts too short")]
    TooShort,
    #[error("unknown non-empty extra_data block artifact")]
    UnknownExtraDataArtifact,
    #[error("unsupported block artifact version: {version}")]
    UnsupportedVersion { version: u8 },
    #[error("committee pre-announce payload too short for epoch")]
    CommitteePreAnnouncePayloadTooShort,
    #[error("boundary TEE expiry exclusions commitment mismatch")]
    BoundaryTeeExpiryExclusionsCommitmentMismatch,
    #[error("boundary header artifact payload too short")]
    BoundaryPayloadTooShort,
    #[error("boundary address list length overflow")]
    BoundaryAddressListLengthOverflow,
    #[error("invalid boundary header artifact payload length: {actual} < {needed}")]
    BoundaryPayloadLengthTooShort { actual: usize, needed: usize },
    #[error("invalid outcome length bytes")]
    InvalidOutcomeLengthBytes,
    #[error("boundary payload length overflow")]
    BoundaryPayloadLengthOverflow,
    #[error("unexpected EOF reading {name}")]
    UnexpectedEofReading { name: &'static str },
    #[error("invalid {name} bytes")]
    InvalidFieldBytes { name: &'static str },
    #[error("invalid vrf group pk length bytes")]
    InvalidVrfGroupPkLengthBytes,
    #[error("invalid boundary header artifact: missing tee recipient count")]
    MissingTeeRecipientCount,
    #[error("tee recipient pubkeys length overflow")]
    TeeRecipientPubkeysLengthOverflow,
    #[error("TEE expiry exclusions length overflow")]
    TeeExpiryExclusionsLengthOverflow,
    #[error("invalid boundary header artifact payload length: {actual} != {expected}")]
    BoundaryPayloadLengthMismatch { actual: usize, expected: usize },
    #[error("invalid boundary {field} flag: {value}")]
    InvalidBoundaryFlag { field: &'static str, value: u8 },
    #[error("too many late finalize batches: {count} > {max_count}")]
    TooManyLateFinalizeBatches { count: usize, max_count: usize },
    #[error("late finalize batches not in strictly ascending canonical order")]
    LateFinalizeBatchesNotCanonicalOrder,
    #[error("late finalize bitmap too long: {len} > {max_len}")]
    LateFinalizeBitmapTooLong { len: usize, max_len: usize },
    #[error("late finalize credits payload too short")]
    LateFinalizePayloadTooShort,
    #[error("trailing bytes in late finalize credits payload")]
    TrailingBytesLateFinalizePayload,
    #[error("truncated late finalize credit prefix")]
    TruncatedLateFinalizeCreditPrefix,
    #[error("late finalize credit overflow")]
    LateFinalizeCreditOverflow,
    #[error("truncated late finalize credit body")]
    TruncatedLateFinalizeCreditBody,
    #[error("invalid execution summary artifact length: {len}")]
    InvalidExecutionSummaryLength { len: usize },
    #[error("timestamp_millis_part payload length: {len} (expected {expected})")]
    InvalidTimestampMillisPartLength { len: usize, expected: usize },
    #[error("compressed-entities root payload length: {len} (expected {expected})")]
    InvalidCompressedEntitiesRootLength { len: usize, expected: usize },
    #[error("duplicate execution summary artifact")]
    DuplicateExecutionSummary,
    #[error("duplicate consensus header artifact")]
    DuplicateConsensusHeader,
    #[error("duplicate timestamp_millis_part")]
    DuplicateTimestampMillisPart,
    #[error("duplicate late finalize credits artifact")]
    DuplicateLateFinalizeCredits,
    #[error("duplicate compressed-entities root artifact")]
    DuplicateCompressedEntitiesRoot,
    #[error("unsupported block artifact tag: {tag}")]
    UnsupportedTag { tag: u8 },
}

impl From<ReshareArtifactError> for PrecompileError {
    fn from(err: ReshareArtifactError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::math`] helpers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PrimitivesMathError {
    #[error("unsupported asset decimals")]
    UnsupportedAssetDecimals,
    #[error("principal and valuation must be positive")]
    PrincipalAndValuationMustBePositive,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    #[error("value rounds to zero")]
    ValueRoundsToZero,
    #[error("scaled_math: multiplication overflow")]
    ScaledMathMultiplicationOverflow,
    #[error("scaled_math: division by zero")]
    ScaledMathDivisionByZero,
    #[error("scaled_math: ceiling overflow")]
    ScaledMathCeilingOverflow,
    #[error("lb_math: log2 i256 conversion")]
    LbMathLog2I256Conversion,
    #[error("lb_math: log2 underflow (x == 0)")]
    LbMathLog2Underflow,
    #[error("lb_math: log2 n widen")]
    LbMathLog2NWiden,
    #[error("lb_math: pow exponent out of LB range (|y| = {abs_y})")]
    LbMathPowExponentOutOfRange { abs_y: u32 },
    #[error("lb_math: pow underflow (x = {x}, y = {y})")]
    LbMathPowUnderflow { x: U256, y: i32 },
    #[error("lb_math: mul_div by zero")]
    LbMathMulDivByZero,
    #[error("lb_math: mul_div overflow")]
    LbMathMulDivOverflow,
    #[error("lb_math: mul_shift overflow")]
    LbMathMulShiftOverflow,
    #[error("lb_math: log2(base) == 0")]
    LbMathLog2BaseZero,
    #[error("lb_math: REAL_ID_SHIFT widen")]
    LbMathRealIdShiftWiden,
    #[error("lb_math: id widen")]
    LbMathIdWiden,
}

impl From<PrimitivesMathError> for PrecompileError {
    fn from(err: PrimitivesMathError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::storage`] providers, handles, and collections.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PrimitivesStorageError {
    #[error("mutation {operation} did not stop the call with the injected failure")]
    InjectedMutationDidNotStop { operation: usize },
    #[error("injected storage mutation failure for owner {address}")]
    InjectedMutationFailureForOwner { address: Address },
    #[error("injected storage mutation failure before operation {operation}")]
    InjectedMutationFailureBeforeOperation { operation: usize },
    #[error("injected storage mutation failure after operation {operation}")]
    InjectedMutationFailureAfterOperation { operation: usize },
    #[error("basic({address}): {reason}")]
    BasicRead { address: Address, reason: String },
    #[error("storage({address},{key}): {reason}")]
    StorageSlotRead {
        address: Address,
        key: U256,
        reason: String,
    },
    #[error("block_hash({number}) failed: {reason}")]
    BlockHashRead { number: u64, reason: String },
    #[error("transfer failed: {reason}")]
    TransferFailed { reason: String },
    #[error("balance_incr failed: {reason}")]
    BalanceIncrFailed { reason: String },
    #[error("burn account load failed: {reason}")]
    BurnAccountLoadFailed { reason: String },
    #[error("burn set_balance failed: {reason}")]
    BurnSetBalanceFailed { reason: String },
    #[error("test provider has no matching Metadosis mutation lease")]
    TestProviderNoMatchingMetadosisLease,
    #[error("test provider has no active Metadosis mutation lease")]
    TestProviderNoActiveMetadosisLease,
    #[error("test provider Metadosis mutation lease identity mismatch")]
    TestProviderMetadosisLeaseIdentityMismatch,
    #[error("test provider has no certified Lysis activation lease")]
    TestProviderNoCertifiedLysisActivationLease,
    #[error("test provider Lysis activation lease identity mismatch")]
    TestProviderLysisActivationLeaseIdentityMismatch,
    #[error("insufficient balance: {from} has {balance} but needs {needed}")]
    InsufficientBalance {
        from: Address,
        balance: U256,
        needed: U256,
    },
    #[error("insufficient balance for burn: {address} has {balance} but needs {needed}")]
    InsufficientBalanceForBurn {
        address: Address,
        balance: U256,
        needed: U256,
    },
    #[error("balance overflow: crediting {amount} to {to} with balance {balance}")]
    CreditBalanceOverflow {
        amount: U256,
        to: Address,
        balance: U256,
    },
    #[error("balance overflow: increasing {address} by {amount} from {balance}")]
    IncreaseBalanceOverflow {
        address: Address,
        amount: U256,
        balance: U256,
    },
    #[error("insufficient balance for transfer from {from}: needs {needed}")]
    InsufficientBalanceForTransfer { from: Address, needed: U256 },
    #[error("insufficient balance for burn from {address}: has {balance} but needs {needed}")]
    InsufficientBalanceForBurnFrom {
        address: Address,
        balance: U256,
        needed: U256,
    },
    #[error("Metadosis mutation frame is not available")]
    MetadosisMutationFrameUnavailable,
    #[error("certified Lysis activation frame is not available")]
    CertifiedLysisActivationFrameUnavailable,
    #[error("{name} not found")]
    RecordNotFound { name: String },
    #[error("{name} already exists")]
    RecordAlreadyExists { name: String },
    #[error("storage provider is already mutably borrowed")]
    ProviderAlreadyMutablyBorrowed,
    #[error("certified Lysis activation frame returned before terminal completion")]
    CertifiedLysisActivationIncomplete,
    #[error("certified Lysis activation {owner} cursor is out of order")]
    CertifiedLysisActivationCursorOutOfOrder { owner: &'static str },
    #[error("array index out of bounds")]
    ArrayIndexOutOfBounds,
    #[error("vec index out of bounds")]
    VecIndexOutOfBounds,
    #[error("circular buffer not initialized")]
    CircularBufferNotInitialized,
    #[error("{0}")]
    EvmInternals(String),
}

impl PrimitivesStorageError {
    /// Convert an EVM internals error into a [`PrecompileError::Storage`].
    pub(crate) fn evm_internals(error: impl std::fmt::Display) -> PrecompileError {
        Self::EvmInternals(error.to_string()).into()
    }
}

impl From<PrimitivesStorageError> for PrecompileError {
    fn from(err: PrimitivesStorageError) -> Self {
        match err {
            PrimitivesStorageError::InjectedMutationDidNotStop { .. }
            | PrimitivesStorageError::InjectedMutationFailureForOwner { .. }
            | PrimitivesStorageError::InjectedMutationFailureBeforeOperation { .. }
            | PrimitivesStorageError::InjectedMutationFailureAfterOperation { .. }
            | PrimitivesStorageError::BasicRead { .. }
            | PrimitivesStorageError::StorageSlotRead { .. }
            | PrimitivesStorageError::BlockHashRead { .. }
            | PrimitivesStorageError::TransferFailed { .. }
            | PrimitivesStorageError::BalanceIncrFailed { .. }
            | PrimitivesStorageError::BurnAccountLoadFailed { .. }
            | PrimitivesStorageError::BurnSetBalanceFailed { .. }
            | PrimitivesStorageError::EvmInternals(_) => Self::Storage(err.to_string()),
            _ => Self::Revert(err.to_string()),
        }
    }
}

/// Errors returned while decoding a Phase 1 signed transaction envelope.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Phase1TxDecodeError {
    #[error("phase1 tx has {trailing_bytes} trailing bytes after EIP-2718 envelope")]
    TrailingBytesAfterEnvelope { trailing_bytes: usize },
}

impl From<Phase1TxDecodeError> for crate::system_tx::SystemTxError {
    fn from(error: Phase1TxDecodeError) -> Self {
        Self::Phase1TxDecode(error.to_string())
    }
}

/// Typed non-canonical reasons for [`crate::tee_attestation_v1::CodecError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TeeCodecNonCanonicalError {
    #[error("empty SGX quote")]
    EmptySgxQuote,
    #[error("empty DCAP collateral component")]
    EmptyDcapCollateralComponent,
    #[error("DirectDev evidence version")]
    DirectDevEvidenceVersion,
    #[error("non-normative block gas limits")]
    NonNormativeBlockGasLimits,
    #[error("node id length")]
    NodeIdLength,
    #[error("descriptor chain id exceeds u64")]
    DescriptorChainIdExceedsU64,
    #[error("empty DKG participant set")]
    EmptyDkgParticipantSet,
    #[error("zero DKG participant set hash")]
    ZeroDkgParticipantSetHash,
    #[error("non-canonical DKG ceremony id")]
    NonCanonicalDkgCeremonyId,
    #[error("invalid TEE lease policy")]
    InvalidTeeLeasePolicy,
    #[error("TEE policy schedule is empty")]
    TeePolicyScheduleEmpty,
    #[error("cached policy hash mismatch")]
    CachedPolicyHashMismatch,
    #[error("policy predecessor hash mismatch")]
    PolicyPredecessorHashMismatch,
    #[error("transition key-ready proof signature")]
    TransitionKeyReadyProofSignature,
    #[error("TEE bootstrap evidence does not contain exactly eight components")]
    TeeBootstrapEvidenceNotEightComponents,
    #[error("invalid TeeBootstrapV2 magic")]
    InvalidTeeBootstrapV2Magic,
    #[error("TEE bootstrap quote is empty")]
    TeeBootstrapQuoteEmpty,
}

impl TeeCodecNonCanonicalError {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptySgxQuote => "empty SGX quote",
            Self::EmptyDcapCollateralComponent => "empty DCAP collateral component",
            Self::DirectDevEvidenceVersion => "DirectDev evidence version",
            Self::NonNormativeBlockGasLimits => "non-normative block gas limits",
            Self::NodeIdLength => "node id length",
            Self::DescriptorChainIdExceedsU64 => "descriptor chain id exceeds u64",
            Self::EmptyDkgParticipantSet => "empty DKG participant set",
            Self::ZeroDkgParticipantSetHash => "zero DKG participant set hash",
            Self::NonCanonicalDkgCeremonyId => "non-canonical DKG ceremony id",
            Self::InvalidTeeLeasePolicy => "invalid TEE lease policy",
            Self::TeePolicyScheduleEmpty => "TEE policy schedule is empty",
            Self::CachedPolicyHashMismatch => "cached policy hash mismatch",
            Self::PolicyPredecessorHashMismatch => "policy predecessor hash mismatch",
            Self::TransitionKeyReadyProofSignature => "transition key-ready proof signature",
            Self::TeeBootstrapEvidenceNotEightComponents => {
                "TEE bootstrap evidence does not contain exactly eight components"
            }
            Self::InvalidTeeBootstrapV2Magic => "invalid TeeBootstrapV2 magic",
            Self::TeeBootstrapQuoteEmpty => "TEE bootstrap quote is empty",
        }
    }
}

impl From<TeeCodecNonCanonicalError> for crate::tee_attestation_v1::CodecError {
    fn from(error: TeeCodecNonCanonicalError) -> Self {
        Self::NonCanonical(error.as_str())
    }
}

/// Errors returned by [`crate::participation`] encoding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParticipationError {
    #[error("validators/signers length mismatch: {validators_len} vs {signers_len}")]
    ValidatorsSignersLengthMismatch {
        validators_len: usize,
        signers_len: usize,
    },
    #[error("validator set too large for participation encoding: {len}")]
    ValidatorSetTooLarge { len: usize },
    #[error("missed proposer list too large for participation encoding: {len}")]
    MissedProposerListTooLarge { len: usize },
    #[error("byzantine validator list too large for participation encoding: {len}")]
    ByzantineValidatorListTooLarge { len: usize },
    #[error(
        "participation metadata exceeds extra_data budget: {total_len} > {max_extra_data_size}"
    )]
    ExceedsExtraDataBudget {
        total_len: usize,
        max_extra_data_size: usize,
    },
}

impl From<ParticipationError> for PrecompileError {
    fn from(err: ParticipationError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::call_bins`] index mutations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CallBinError {
    #[error("call bin index: entry is already indexed")]
    EntryAlreadyIndexed,
    #[error("call bin index: bin entry count overflow")]
    BinEntryCountOverflow,
    #[error("call bin index: slot does not hold its entry")]
    SlotDoesNotHoldEntry,
    #[error("call bin index: slot past the end of its bin")]
    SlotPastEndOfBin,
}

impl From<CallBinError> for PrecompileError {
    fn from(err: CallBinError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::dispatch`] input and payable-selector checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DispatchInputError {
    #[error("invalid input: missing function selector")]
    MissingFunctionSelector,
    #[error("decode error: {reason}")]
    DecodeError { reason: String },
    #[error("invalid ABI bytes argument")]
    InvalidAbiBytesArgument,
    #[error("invalid bytes length: expected {expected_len}")]
    InvalidBytesLength { expected_len: usize },
    #[error("payable selector is not declared in PAYABLE_SELECTORS")]
    UndeclaredPayableSelector,
    #[error("non-payable function called with value")]
    NonPayableCalledWithValue,
}

impl From<DispatchInputError> for PrecompileError {
    fn from(err: DispatchInputError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Errors returned by [`crate::tee_signatures`] signature recovery.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TeeSignatureError {
    #[error("recoverable signature has invalid recovery id {recid_byte}")]
    InvalidRecoveryId { recid_byte: u8 },
    #[error("recoverable signature is malformed: {reason}")]
    MalformedRecoverableSignature { reason: String },
    #[error("signature recovery failed: {reason}")]
    SignatureRecoveryFailed { reason: String },
    #[error("recovered compressed public key is not 33 bytes")]
    InvalidCompressedPublicKeyLength,
    #[error("recovered key decode failed: {reason}")]
    RecoveredKeyDecodeFailed { reason: String },
}

impl From<TeeSignatureError> for PrecompileError {
    fn from(err: TeeSignatureError) -> Self {
        Self::Revert(err.to_string())
    }
}

impl From<CurrencyCodeError> for PrecompileError {
    fn from(value: CurrencyCodeError) -> Self {
        Self::Revert(value.to_string())
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
            | Self::EnclaveUnavailable(_)
            | Self::SubCall(SubCallError::DatabaseError(_)) => true,
            Self::OutOfGas
            | Self::BodyReadCorruption(_)
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
            Self::OutOfGas => SweepFailure::Stop,
            _ => SweepFailure::Skip,
        }
    }

    /// As [`Self::sweep_failure`], for a sweep over stored records. The right's own
    /// index checks revert, so a body corruption reaching the sweep is this node's store.
    pub fn body_sweep_failure(&self) -> SweepFailure {
        match self {
            Self::BodyReadCorruption(_) => SweepFailure::Propagate,
            other => other.sweep_failure(),
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

    #[test]
    fn a_body_sweep_fails_the_block_on_a_corrupt_body() {
        let corrupt = PrecompileError::BodyReadCorruption("x".into());
        assert_eq!(corrupt.body_sweep_failure(), SweepFailure::Propagate);
        assert_eq!(
            PrecompileError::Revert("x".into()).body_sweep_failure(),
            SweepFailure::Skip
        );
        assert_eq!(
            PrecompileError::OutOfGas.body_sweep_failure(),
            SweepFailure::Stop
        );
    }
}
