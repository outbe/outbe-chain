//! Typed errors for Outbe EVM setup, dispatch, and blocks.

use alloy_primitives::{Address, B256, U256};
use core::fmt::Display;
use outbe_metadosis::api::OcompFinalityAuthorityError;
use outbe_primitives::{
    error::PrecompileError, storage::MetadosisMutationEntitlements,
    storage::MetadosisMutationPurposeTag,
};
use std::path::Path;
use std::sync::Arc;

/// Checks whether a precompile error is a user revert.
#[must_use]
pub(crate) const fn is_revert_error(error: &PrecompileError) -> bool {
    matches!(
        error,
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_)
    )
}

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

/// Errors that occur when the builder constructs the Outbe EVM setup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutbeEvmBuilderError {
    #[error("OCOMP fork install requires an active lifecycle schedule")]
    OcompForkInstallRequiresLifecycleSchedule,
    #[error("active OCOMP lifecycle requires a chain-manifest fork install")]
    ActiveOcompLifecycleRequiresForkInstall,
    #[error(
        "OCOMP fork install height {install_height} differs from lifecycle height {lifecycle_height}"
    )]
    OcompForkInstallHeightMismatch {
        install_height: u64,
        lifecycle_height: u64,
    },
    #[error("live Outbe EVM construction requires RuntimeBodyReaders")]
    MissingRuntimeBodyReaders,
    #[error("live Outbe EVM construction requires CompressedTreeService")]
    MissingCompressedTreeService,
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

    pub(crate) fn invalid_chain_spec_activation(error: &str) -> Self {
        Self::InvalidChainSpecActivation(error.to_owned())
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

/// Errors that occur when the config binds the parent tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompressedEntitiesScopeConfigError {
    #[error("{0}")]
    ConfigureParentTreeFactory(String),
}

impl CompressedEntitiesScopeConfigError {
    pub(crate) fn from_display(error: impl Display) -> Self {
        Self::ConfigureParentTreeFactory(error.to_string())
    }
}

/// Errors that occur when the finality authority checks local finalized headers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FinalityAuthorityLocalError {
    #[error("{0}")]
    Provider(String),
    #[error("finalized checkpoint is unavailable")]
    FinalizedCheckpointUnavailable,
    #[error("claimed request height {claimed} is above local finalized height {finalized}")]
    ClaimedHeightAboveFinalized { claimed: u64, finalized: u64 },
    #[error("canonical hash at finalized height {height} is unavailable")]
    CanonicalHashUnavailable { height: u64 },
}

impl FinalityAuthorityLocalError {
    pub(crate) fn provider(error: impl Display) -> OcompFinalityAuthorityError {
        Self::Provider(error.to_string()).into()
    }
}

impl From<FinalityAuthorityLocalError> for OcompFinalityAuthorityError {
    fn from(error: FinalityAuthorityLocalError) -> Self {
        Self::LocalAuthority(error.to_string())
    }
}

/// Errors that occur when the config recovers signers from system transactions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SystemTxExpectationError {
    #[error("recover system tx signer: {0}")]
    RecoverSigner(String),
}

impl SystemTxExpectationError {
    pub(crate) fn recover_signer(error: impl Display) -> Self {
        Self::RecoverSigner(error.to_string())
    }
}

/// Errors that occur when the handler checks a stablecoin vote target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StablecoinVoteTargetError {
    #[error("non-canonical stablecoin payload")]
    NonCanonicalPayload,
    #[error("stablecoin proposer must equal issuer")]
    ProposerMustEqualIssuer,
    #[error("invalid stablecoin identity")]
    InvalidIdentity,
}

impl From<StablecoinVoteTargetError> for PrecompileError {
    fn from(error: StablecoinVoteTargetError) -> Self {
        Self::Revert(error.to_string())
    }
}

/// Errors that occur when a route dispatches a precompile call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrecompileRouteError {
    #[error("execution body read authority was not supplied")]
    MissingBodyReadAuthority,
    #[error("unregistered stablecoin address")]
    UnregisteredStablecoinAddress,
    #[error("stablecoin token cannot receive native value")]
    StablecoinCannotReceiveNativeValue,
    #[error("stablecoin registry unavailable")]
    StablecoinRegistryUnavailable,
    #[error("stablecoin Factory reverse id does not derive callee {token}")]
    StablecoinReverseIdMismatch { token: Address },
    #[error("stablecoin {token} marker code mismatch")]
    StablecoinMarkerCodeMismatch { token: Address },
    #[error("stablecoin {token} schema is not initialized for V1")]
    StablecoinSchemaNotInitialized { token: Address },
    #[error("stablecoin {token} identity does not match Factory registration")]
    StablecoinIdentityMismatch { token: Address },
}

impl From<PrecompileRouteError> for PrecompileError {
    fn from(error: PrecompileRouteError) -> Self {
        match error {
            PrecompileRouteError::MissingBodyReadAuthority => {
                Self::BodyReadUnavailable(error.to_string())
            }
            PrecompileRouteError::UnregisteredStablecoinAddress
            | PrecompileRouteError::StablecoinCannotReceiveNativeValue
            | PrecompileRouteError::StablecoinRegistryUnavailable
            | PrecompileRouteError::StablecoinReverseIdMismatch { .. }
            | PrecompileRouteError::StablecoinMarkerCodeMismatch { .. }
            | PrecompileRouteError::StablecoinSchemaNotInitialized { .. }
            | PrecompileRouteError::StablecoinIdentityMismatch { .. } => {
                Self::Revert(error.to_string())
            }
        }
    }
}

/// Errors that occur in the canonical ZK verifier precompile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ZkPrecompileError {
    #[error("{0}")]
    DecodeVerifyCall(String),
    #[error("zk verification backend failed: {0}")]
    BackendFailed(String),
}

impl ZkPrecompileError {
    pub(crate) fn decode_verify_call(error: impl Display) -> PrecompileError {
        Self::DecodeVerifyCall(error.to_string()).into()
    }

    pub(crate) fn backend_failed(error: impl Display) -> PrecompileError {
        Self::BackendFailed(error.to_string()).into()
    }
}

impl From<ZkPrecompileError> for PrecompileError {
    fn from(error: ZkPrecompileError) -> Self {
        Self::Revert(error.to_string())
    }
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
    #[error("OcompLifecycleBegin requires the active compressed-entity scope")]
    OcompLifecycleBeginMissingScope,
    #[error("OCOMP terminal request requires execution seal authority")]
    OcompTerminalRequestMissingSealAuthority,
    #[error("{name} exceeds u64 range: {value}")]
    ValueExceedsU64 { name: &'static str, value: U256 },
    #[error("Cycle execution body read authority was not supplied")]
    MissingCycleBodyReadAuthority,
    #[error("RewardsGemDelivery execution body read authority was not supplied")]
    MissingRewardsGemDeliveryBodyReadAuthority,
    #[error("proposer is not a current consensus participant: {proposer}")]
    ProposerNotConsensusParticipant { proposer: Address },
    #[error(
        "missing preloaded execution summary for finalized block {block_number} ({block_hash})"
    )]
    MissingPreloadedExecutionSummary { block_number: u64, block_hash: B256 },
    #[error("certified Metadosis finality requires the verified parent state root")]
    MissingVerifiedParentStateRoot,
    #[error(
        "CertifiedParentAccounting identical replay at unexpected progress: last_accounted={last_accounted}, expected={expected}"
    )]
    IdenticalReplayUnexpectedProgress { last_accounted: u64, expected: u64 },
    #[error(
        "CertifiedParentAccounting progress gap: last_accounted={last_accounted}, expected_previous={expected_previous}, accounting_parent={accounting_parent}"
    )]
    ProgressGap {
        last_accounted: u64,
        expected_previous: u64,
        accounting_parent: u64,
    },
    #[error("CertifiedParentAccounting system tx requires block_number >= 2")]
    RequiresBlockNumberAtLeastTwo,
    #[error("block number underflow")]
    BlockNumberUnderflow,
    #[error(
        "CertifiedParentAccounting metadata must target immediate parent: expected {expected}, got {actual}"
    )]
    TargetImmediateParentMismatch { expected: u64, actual: u64 },
    #[error("CertifiedParentAccounting metadata has zero finalized block hash")]
    ZeroFinalizedBlockHash,
    #[error(
        "LateFinalizeCredits: epoch mismatch for fb_number {fb_number} (escrow {escrowed_epoch}, credit {credit_epoch})"
    )]
    LateCreditEpochMismatch {
        fb_number: u64,
        escrowed_epoch: u64,
        credit_epoch: u64,
    },
    #[error(
        "LateFinalizeCredits: committee_set_hash mismatch for fb_number {fb_number} (escrow {escrowed_csh}, credit {credit_csh})"
    )]
    LateCreditCommitteeSetHashMismatch {
        fb_number: u64,
        escrowed_csh: B256,
        credit_csh: B256,
    },
    #[error(
        "LateFinalizeCredits: view mismatch for fb_number {fb_number} (escrow {escrowed_view}, credit {credit_view})"
    )]
    LateCreditViewMismatch {
        fb_number: u64,
        escrowed_view: u64,
        credit_view: u64,
    },
    #[error(
        "LateFinalizeCredits: parent_view mismatch for fb_number {fb_number} (escrow {escrowed_parent_view}, credit {credit_parent_view})"
    )]
    LateCreditParentViewMismatch {
        fb_number: u64,
        escrowed_parent_view: u64,
        credit_parent_view: u64,
    },
    #[error("LateFinalizeCredits: no escrow for fb_number {fb_number} (credit fb_hash {fb_hash})")]
    LateCreditMissingEscrow { fb_number: u64, fb_hash: B256 },
    #[error(
        "LateFinalizeCredits: fb_hash mismatch for fb_number {fb_number} (escrow {escrowed_hash}, credit {credit_hash})"
    )]
    LateCreditFbHashMismatch {
        fb_number: u64,
        escrowed_hash: B256,
        credit_hash: B256,
    },
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
    #[error(
        "LateFinalizeCredits: missing committee snapshot for epoch={epoch} key={snapshot_key}"
    )]
    LateCreditMissingCommitteeSnapshot { epoch: u64, snapshot_key: B256 },
    #[error("LateFinalizeCredits: proof verify failed for fb={fb_hash}: {reason}")]
    LateCreditProofVerifyFailed { fb_hash: B256, reason: String },
    #[error("LateFinalizeCredits: signer index {idx} out of committee range")]
    LateCreditSignerIndexOutOfRange { idx: usize },
    #[error("invalid TEE ChainSpec authority: {0}")]
    InvalidTeeChainSpecAuthority(String),
    #[error("OST3 is not active at the current block")]
    Ost3NotActive,
    #[error("invalid active TEE policy: {0}")]
    InvalidActiveTeePolicy(String),
    #[error("OST3 policy does not match the active ChainSpec schedule")]
    Ost3PolicyScheduleMismatch,
    #[error("TeeBootstrapV2: registry already bootstrapped")]
    TeeRegistryAlreadyBootstrapped,
    #[error(
        "TeeBootstrapV2: committee snapshot block {snapshot_block} does not equal current block {current_block}"
    )]
    TeeCommitteeSnapshotBlockMismatch {
        snapshot_block: u64,
        current_block: u64,
    },
    #[error("TeeBootstrapV2: payload policy is not the authoritative active V1 policy")]
    TeePayloadPolicyNotAuthoritative,
    #[error("canonical TeeBootstrapV2 cannot produce its signing hash: {0}")]
    TeeCannotProduceSigningHash(String),
    #[error("TeeBootstrapV2: invalid committee signature for {validator}")]
    TeeInvalidCommitteeSignature { validator: Address },
    #[error("TeeBootstrapV2: epoch-0 committee snapshot is missing")]
    TeeMissingEpoch0CommitteeSnapshot,
    #[error("TeeBootstrapV2: committee snapshot hash mismatch")]
    TeeCommitteeSnapshotHashMismatch,
    #[error("canonical TeeBootstrapV2 cannot reconstruct evidence {index}: {reason}")]
    TeeCannotReconstructEvidence { index: usize, reason: String },
    #[error("authoritative TeeBootstrapV2 policy cannot be hashed: {0}")]
    TeeCannotHashAuthoritativePolicy(String),
    #[error("TeeBootstrapV2: active consensus committee is empty")]
    TeeEmptyActiveConsensusCommittee,
    #[error("TeeBootstrapV2: participants must equal the complete active consensus committee")]
    TeeParticipantsMismatchActiveCommittee,
}

impl BeginBlockPrecompileError {
    pub(crate) fn invalid_system_tx_input(error: impl Display) -> PrecompileError {
        Self::InvalidSystemTxInput(error.to_string()).into()
    }

    pub(crate) fn late_credit_proof_verify_failed(
        fb_hash: B256,
        error: impl Display,
    ) -> PrecompileError {
        Self::LateCreditProofVerifyFailed {
            fb_hash,
            reason: error.to_string(),
        }
        .into()
    }

    pub(crate) fn invalid_tee_chain_spec_authority(error: impl Display) -> PrecompileError {
        Self::InvalidTeeChainSpecAuthority(error.to_string()).into()
    }

    pub(crate) fn invalid_active_tee_policy(error: impl Display) -> PrecompileError {
        Self::InvalidActiveTeePolicy(error.to_string()).into()
    }

    pub(crate) fn tee_cannot_produce_signing_hash(error: impl Display) -> PrecompileError {
        Self::TeeCannotProduceSigningHash(error.to_string()).into()
    }

    pub(crate) fn tee_cannot_reconstruct_evidence(
        index: usize,
        error: impl Display,
    ) -> PrecompileError {
        Self::TeeCannotReconstructEvidence {
            index,
            reason: error.to_string(),
        }
        .into()
    }

    pub(crate) fn tee_cannot_hash_authoritative_policy(error: impl Display) -> PrecompileError {
        Self::TeeCannotHashAuthoritativePolicy(error.to_string()).into()
    }
}

impl From<BeginBlockPrecompileError> for PrecompileError {
    fn from(error: BeginBlockPrecompileError) -> Self {
        match error {
            BeginBlockPrecompileError::MissingPreloadedExecutionSummary { .. }
            | BeginBlockPrecompileError::OcompLifecycleBeginMissingScope
            | BeginBlockPrecompileError::OcompTerminalRequestMissingSealAuthority
            | BeginBlockPrecompileError::MissingCycleBodyReadAuthority
            | BeginBlockPrecompileError::MissingRewardsGemDeliveryBodyReadAuthority
            | BeginBlockPrecompileError::MissingVerifiedParentStateRoot => {
                Self::BodyReadUnavailable(error.to_string())
            }
            BeginBlockPrecompileError::InvalidTeeChainSpecAuthority(_)
            | BeginBlockPrecompileError::InvalidActiveTeePolicy(_)
            | BeginBlockPrecompileError::InvalidSystemCaller
            | BeginBlockPrecompileError::NativeValueRejected
            | BeginBlockPrecompileError::InvalidSystemTxInput(_)
            | BeginBlockPrecompileError::ValueExceedsU64 { .. }
            | BeginBlockPrecompileError::ProposerNotConsensusParticipant { .. }
            | BeginBlockPrecompileError::IdenticalReplayUnexpectedProgress { .. }
            | BeginBlockPrecompileError::ProgressGap { .. }
            | BeginBlockPrecompileError::RequiresBlockNumberAtLeastTwo
            | BeginBlockPrecompileError::BlockNumberUnderflow
            | BeginBlockPrecompileError::TargetImmediateParentMismatch { .. }
            | BeginBlockPrecompileError::ZeroFinalizedBlockHash
            | BeginBlockPrecompileError::LateCreditEpochMismatch { .. }
            | BeginBlockPrecompileError::LateCreditCommitteeSetHashMismatch { .. }
            | BeginBlockPrecompileError::LateCreditViewMismatch { .. }
            | BeginBlockPrecompileError::LateCreditParentViewMismatch { .. }
            | BeginBlockPrecompileError::LateCreditMissingEscrow { .. }
            | BeginBlockPrecompileError::LateCreditFbHashMismatch { .. }
            | BeginBlockPrecompileError::LateCreditFbNumberAtOrAboveBlock { .. }
            | BeginBlockPrecompileError::LateCreditOutsideInclusionWindow { .. }
            | BeginBlockPrecompileError::LateCreditInclusionDistanceExceedsU8 { .. }
            | BeginBlockPrecompileError::LateCreditMissingCommitteeSnapshot { .. }
            | BeginBlockPrecompileError::LateCreditProofVerifyFailed { .. }
            | BeginBlockPrecompileError::LateCreditSignerIndexOutOfRange { .. }
            | BeginBlockPrecompileError::Ost3NotActive
            | BeginBlockPrecompileError::Ost3PolicyScheduleMismatch
            | BeginBlockPrecompileError::TeeRegistryAlreadyBootstrapped
            | BeginBlockPrecompileError::TeeCommitteeSnapshotBlockMismatch { .. }
            | BeginBlockPrecompileError::TeePayloadPolicyNotAuthoritative
            | BeginBlockPrecompileError::TeeCannotProduceSigningHash(_)
            | BeginBlockPrecompileError::TeeInvalidCommitteeSignature { .. }
            | BeginBlockPrecompileError::TeeMissingEpoch0CommitteeSnapshot
            | BeginBlockPrecompileError::TeeCommitteeSnapshotHashMismatch
            | BeginBlockPrecompileError::TeeCannotReconstructEvidence { .. }
            | BeginBlockPrecompileError::TeeCannotHashAuthoritativePolicy(_)
            | BeginBlockPrecompileError::TeeEmptyActiveConsensusCommittee
            | BeginBlockPrecompileError::TeeParticipantsMismatchActiveCommittee => {
                Self::Revert(error.to_string())
            }
        }
    }
}

/// Errors that occur in executor hooks, boundary updates, and genesis checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutorHookError {
    #[error("consensus metadata signer bitmap length mismatch")]
    SignerBitmapLengthMismatch,
    #[error("consensus metadata committee contains duplicate addresses")]
    DuplicateCommitteeAddresses,
    #[error("consensus metadata committee member is not a registered validator: {address}")]
    UnregisteredCommitteeMember { address: Address },
    #[error(
        "consensus metadata missed proposer is not in finalized committee: {validator} (view {view})"
    )]
    MissedProposerNotInCommittee { validator: Address, view: u64 },
    #[error("consensus metadata signer bitmap contains non-binary entry")]
    NonBinarySignerBitmapEntry,
    #[error(
        "boundary TEE expiry exclusions commitment mismatch: expected {expected}, got {actual}"
    )]
    TeeExpiryExclusionsHashMismatch { expected: B256, actual: B256 },
    #[error("boundary active_set_hash mismatch: expected {expected}, got {actual}")]
    ActiveSetHashMismatch { expected: B256, actual: B256 },
    #[error("boundary VRF group public key hash mismatch: expected {expected}, got {actual}")]
    VrfGroupPublicKeyHashMismatch { expected: B256, actual: B256 },
    #[error("boundary committee_set_hash mismatch: expected {expected}, got {actual}")]
    CommitteeSetHashMismatch { expected: B256, actual: B256 },
    #[error("ValidatorSet epoch exceeds u64")]
    ValidatorSetEpochExceedsU64,
    #[error(
        "boundary active_set_hash changed without validator-set change: current={current}, boundary={boundary}"
    )]
    ActiveSetHashChangedWithoutValidatorSetChange { current: B256, boundary: B256 },
    #[error("ValidatorSet epoch overflow at BoundaryOutcome")]
    ValidatorSetEpochOverflow,
    #[error(
        "BoundaryOutcome epoch must bootstrap epoch 0 at block 1 or activate current+1: current={current}, incoming={incoming}, block={block}"
    )]
    InvalidBoundaryEpochTransition {
        current: u64,
        incoming: u64,
        block: u64,
    },
    #[error("boundary active set contains unregistered validator {address}")]
    UnregisteredBoundaryValidator { address: Address },
    #[error("compressed-entities SealOutput/EVM root mismatch: seal={seal}, evm={evm}")]
    SealOutputEvmRootMismatch { seal: B256, evm: B256 },
    #[error("Staking min_stake must be initialized in genesis")]
    StakingMinStakeUninitialized,
    #[error("genesis validator {address} stake mismatch between ValidatorSet and Staking")]
    GenesisValidatorStakeMismatch { address: Address },
    #[error("genesis total stake overflow")]
    GenesisTotalStakeOverflow,
    #[error("genesis total_staked mismatch: state={state}, expected={expected}")]
    GenesisTotalStakedMismatch { state: U256, expected: U256 },
    #[error("ValidatorSet must be initialized in genesis; executor genesis backfill is disabled")]
    ValidatorSetUninitializedInGenesis,
    #[error("genesis ValidatorSet epoch_length_blocks mismatch: state={state}, genesis={genesis}")]
    GenesisEpochLengthMismatch { state: u32, genesis: u32 },
    #[error(
        "genesis active consensus set size mismatch: state={state}, genesis validators={genesis}"
    )]
    GenesisActiveConsensusCountMismatch { state: u32, genesis: usize },
    #[error("genesis validator {address} is missing from ValidatorSet")]
    MissingGenesisValidator { address: Address },
    #[error("genesis validator {address} consensus pubkey mismatch")]
    GenesisConsensusPubkeyMismatch { address: Address },
    #[error("genesis validator {address} must be active with a BLS share")]
    GenesisValidatorNotActive { address: Address },
    #[error("genesis validator {address} stake below min_stake")]
    GenesisValidatorStakeBelowMinimum { address: Address },
}

impl From<ExecutorHookError> for PrecompileError {
    fn from(error: ExecutorHookError) -> Self {
        match error {
            ExecutorHookError::SignerBitmapLengthMismatch
            | ExecutorHookError::DuplicateCommitteeAddresses
            | ExecutorHookError::UnregisteredCommitteeMember { .. }
            | ExecutorHookError::MissedProposerNotInCommittee { .. }
            | ExecutorHookError::NonBinarySignerBitmapEntry
            | ExecutorHookError::TeeExpiryExclusionsHashMismatch { .. }
            | ExecutorHookError::ActiveSetHashMismatch { .. }
            | ExecutorHookError::VrfGroupPublicKeyHashMismatch { .. }
            | ExecutorHookError::CommitteeSetHashMismatch { .. }
            | ExecutorHookError::ActiveSetHashChangedWithoutValidatorSetChange { .. }
            | ExecutorHookError::InvalidBoundaryEpochTransition { .. }
            | ExecutorHookError::UnregisteredBoundaryValidator { .. } => {
                Self::Revert(error.to_string())
            }
            ExecutorHookError::ValidatorSetEpochExceedsU64
            | ExecutorHookError::ValidatorSetEpochOverflow
            | ExecutorHookError::SealOutputEvmRootMismatch { .. }
            | ExecutorHookError::StakingMinStakeUninitialized
            | ExecutorHookError::GenesisValidatorStakeMismatch { .. }
            | ExecutorHookError::GenesisTotalStakeOverflow
            | ExecutorHookError::GenesisTotalStakedMismatch { .. }
            | ExecutorHookError::ValidatorSetUninitializedInGenesis
            | ExecutorHookError::GenesisEpochLengthMismatch { .. }
            | ExecutorHookError::GenesisActiveConsensusCountMismatch { .. }
            | ExecutorHookError::MissingGenesisValidator { .. }
            | ExecutorHookError::GenesisConsensusPubkeyMismatch { .. }
            | ExecutorHookError::GenesisValidatorNotActive { .. }
            | ExecutorHookError::GenesisValidatorStakeBelowMinimum { .. } => {
                Self::Fatal(error.to_string())
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
pub enum CtxStorageError {
    #[error(
        "Metadosis mutation purpose is not entitled for this EVM call: purpose={purpose:?} binding={binding:#x} remaining={remaining:?}"
    )]
    MetadosisMutationPurposeNotEntitled {
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        remaining: MetadosisMutationEntitlements,
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
