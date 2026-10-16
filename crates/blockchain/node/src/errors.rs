//! This module defines typed errors for `outbe-node`.

use alloy_primitives::{Address, B256};
use outbe_metadosis::config::OcompForkInstallClassification;
use reth_ethereum::consensus::ConsensusError;
use reth_provider::ProviderError;
use std::sync::Arc;

use crate::{
    ocomp::{
        finality::{
            FinalizedIntentProofBuildError, FinalizedIntentVerifierError, HistoricalCommitteeError,
            PublicFinalizedIntentProofBuildError,
        },
        retention::RetentionError,
    },
    tee_remote_session::{ExternalRegistryAdmissionError, LocalRegistryAdmissionError},
};

/// This type alias represents the callback error for the enclave call context.
pub type CallContextError = String;

/// Build a [`ConsensusError::Other`] from a displayable error.
pub fn consensus_display_error(error: impl core::fmt::Display) -> ConsensusError {
    ConsensusError::Other(Arc::<dyn core::error::Error + Send + Sync>::from(Box::<
        dyn core::error::Error + Send + Sync,
    >::from(
        error.to_string(),
    )))
}

/// Typed consensus policy and system-transaction validation errors.
#[derive(Debug, thiserror::Error)]
pub enum ConsensusPolicyError {
    #[error("block {block_number} protocol gas limit mismatch: expected {expected}, got {actual}")]
    ProtocolGasLimitMismatch {
        block_number: u64,
        expected: u64,
        actual: u64,
    },
    #[error(
        "block RLP size {rlp_length} exceeds the {max_block_rlp_bytes}-byte P2P transport cap"
    )]
    BlockRlpSizeExceedsTransportCap {
        rlp_length: usize,
        max_block_rlp_bytes: usize,
    },
    #[error(
        "block timestamp_millis {timestamp} exceeds parent {parent} by {drift} ms, above the {max_drift} ms maximum drift"
    )]
    TimestampExceedsMaxDrift {
        timestamp: u64,
        parent: u64,
        drift: u64,
        max_drift: u64,
    },
    #[error(
        "block timestamp_millis {timestamp} advances parent {parent} by only {drift} ms, below the {min_advance} ms minimum advance"
    )]
    TimestampBelowMinAdvance {
        timestamp: u64,
        parent: u64,
        drift: u64,
        min_advance: u64,
    },
    #[error("timestamp_millis_part {part} must be less than {max_part}")]
    InvalidTimestampMillisPart { part: u64, max_part: u64 },
    #[error("non-genesis block beneficiary must be REWARDS_ADDRESS {expected}: got {actual}")]
    NonGenesisBeneficiaryMismatch { expected: Address, actual: Address },
    #[error("decode system tx input: {0}")]
    DecodeSystemTxInput(String),
}

impl ConsensusPolicyError {
    #[must_use]
    pub fn decode_system_tx_input(error: impl core::fmt::Display) -> Self {
        Self::DecodeSystemTxInput(error.to_string())
    }
}

impl From<ConsensusPolicyError> for ConsensusError {
    fn from(error: ConsensusPolicyError) -> Self {
        consensus_display_error(error)
    }
}

/// This enum defines errors that occur when the node reads a finalized frame.
#[derive(Debug, thiserror::Error)]
pub enum FinalizedFrameError {
    #[error("canonical finalized block {height} is unavailable")]
    CanonicalBlockUnavailable { height: u64 },
    #[error("canonical finalized block {height} ({hash}) is unavailable by hash")]
    BlockUnavailableByHash { height: u64, hash: B256 },
    #[error("provider returned finalized block {actual} while height {requested} was requested")]
    BlockHeightMismatch { actual: u64, requested: u64 },
    #[error("finalized block at height {height} recomputed to {actual}, expected {expected}")]
    RecomputedBlockHashMismatch {
        height: u64,
        actual: B256,
        expected: B256,
    },
    #[error(
        "canonical block hash {canonical} conflicts with finalized hash {finalized} at height {height}"
    )]
    FinalizedTargetHashConflict {
        canonical: B256,
        finalized: B256,
        height: u64,
    },
    #[error("receipts for finalized block {height} are unavailable")]
    ReceiptsUnavailable { height: u64 },
    #[error("finalized block {height} has {transactions} transactions but {receipts} receipts")]
    ReceiptCountMismatch {
        height: u64,
        transactions: usize,
        receipts: usize,
    },
}

impl FinalizedFrameError {
    /// Returns true when the error reflects transient provider lag rather than
    /// a canonical history or receipt conflict.
    #[must_use]
    pub const fn is_transient_unavailability(&self) -> bool {
        matches!(
            self,
            Self::CanonicalBlockUnavailable { .. }
                | Self::BlockUnavailableByHash { .. }
                | Self::ReceiptsUnavailable { .. }
        )
    }
}

/// This enum defines errors that [`crate::shutdown::NodeShutdown`] returns.
#[derive(Debug, thiserror::Error)]
pub enum NodeShutdownError {
    #[error("shutdown state poisoned")]
    StatePoisoned,
    #[error("payload service already registered")]
    PayloadServiceAlreadyRegistered,
    #[error("engine exit already observed")]
    EngineExitAlreadyObserved,
    #[error("execution exit observer panicked: {0}")]
    ExecutionExitObserverPanicked(String),
    #[error("consensus thread panicked: {0}")]
    ConsensusThreadPanicked(String),
    #[error("{name} panicked: {message}")]
    TaskPanicked { name: &'static str, message: String },
    #[error("execution engine completion missing after runtime teardown")]
    ExecutionEngineCompletionMissing,
    #[error("additional shutdown failure: {0}")]
    AdditionalShutdownFailure(String),
}

impl NodeShutdownError {
    #[must_use]
    pub fn additional_shutdown_failure(error: &eyre::Report) -> Self {
        Self::AdditionalShutdownFailure(format!("{error:#}"))
    }
}

/// This enum defines errors that occur when the node resolves the enclave call context.
#[derive(Debug, thiserror::Error)]
pub enum TeeCallContextError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("enclave context header unavailable")]
    HeaderUnavailable,
    #[error("enclave context protocol version overflow")]
    ProtocolVersionOverflow,
}

impl TeeCallContextError {
    #[must_use]
    pub fn into_call_context_error(self) -> CallContextError {
        self.to_string()
    }
}

/// Errors returned by read-only [`outbe_primitives::storage::readonly::StorageReader`] adapters in `outbe-node`.
#[derive(Debug, thiserror::Error)]
pub enum NodeStorageReadError {
    #[error("pinned OCOMP snapshot state read failed: {0}")]
    PinnedOcompSnapshotStateReadFailed(#[source] ProviderError),
    #[error("anchored proof reader is restricted to TeeRegistry")]
    AnchoredProofReaderRestrictedToTeeRegistry,
    #[error("finalized state read failed: {0}")]
    FinalizedStateReadFailed(#[source] ProviderError),
}

impl From<NodeStorageReadError> for outbe_primitives::error::PrecompileError {
    fn from(error: NodeStorageReadError) -> Self {
        Self::Storage(error.to_string())
    }
}

/// This enum defines errors that occur when the node validates a canary batch.
#[derive(Debug, thiserror::Error)]
pub enum CanaryBatchValidationError {
    #[error("canary expected 1 result, got {0}")]
    UnexpectedResultCount(usize),
    #[error("canary offer rejected: {0}")]
    OfferRejected(String),
    #[error("canary result echoes a different owner")]
    OwnerMismatch,
    #[error("canary inputs_canonical_hash mismatch (non-determinism)")]
    InputsCanonicalHashMismatch,
    #[error("no pinned attestation key")]
    NoPinnedAttestationKey,
    #[error("canary attestation tag invalid: {0}")]
    InvalidAttestationTag(String),
}

impl CanaryBatchValidationError {
    #[must_use]
    pub fn invalid_attestation_tag(error: impl core::fmt::Display) -> Self {
        Self::InvalidAttestationTag(error.to_string())
    }
}

/// This enum defines errors that occur when the node validates an OCOMP fork install.
#[derive(Debug, thiserror::Error)]
pub enum OcompForkInstallError {
    #[error("invalid genesis config OCOMP fork install: {0}")]
    InvalidGenesisConfig(String),
    #[error("invalid canonical OCOMP fork install: {0}")]
    InvalidCanonicalInstall(String),
    #[error("OCOMP fork install does not bind selected chain: {0}")]
    ChainBindingMismatch(String),
    #[error("hash OCOMP fork install: {0}")]
    HashInstall(String),
    #[error("OCOMP fork install hash mismatch: manifest {manifest}, canonical {canonical}")]
    InstallHashMismatch { manifest: B256, canonical: B256 },
    #[error(
        "fresh-devnet OCOMP install must be Measurement at block {expected_height}, got {classification:?} at {actual_height}"
    )]
    InvalidFreshDevnetInstall {
        expected_height: u64,
        classification: OcompForkInstallClassification,
        actual_height: u64,
    },
    #[error("chain genesis config is missing required {key}")]
    MissingGenesisKey { key: &'static str },
    #[error(
        "fresh-devnet Measurement OCOMP install must activate at block {expected_height}, got {actual_height}"
    )]
    InvalidMeasurementActivationHeight {
        expected_height: u64,
        actual_height: u64,
    },
    #[error("invalid {key}: {reason}")]
    InvalidGenesisField { key: &'static str, reason: String },
    #[error("OCOMP-enabled chain genesis config is missing required {key}")]
    MissingOcompGenesisKey { key: &'static str },
    #[error("OCOMP snapshot retention horizon overflow")]
    SnapshotRetentionHorizonOverflow,
    #[error("OCOMP maximum job lifetime overflow")]
    MaximumJobLifetimeOverflow,
    #[error(
        "OCOMP snapshot retention horizon is too short: ({retain_epochs} - 1) * {epoch_length_blocks} = {retained_epoch_span} must be strictly greater than {awaiting_finality_deadline_blocks} + {min_finality_depth} + {result_deadline_blocks} = {required_job_lifetime}"
    )]
    SnapshotRetentionHorizonTooShort {
        retain_epochs: u64,
        epoch_length_blocks: u64,
        retained_epoch_span: u64,
        awaiting_finality_deadline_blocks: u64,
        min_finality_depth: u64,
        result_deadline_blocks: u64,
        required_job_lifetime: u64,
    },
    #[error("fresh-devnet genesis config is missing required {key}")]
    MissingFreshDevnetGenesisKey { key: &'static str },
    #[error("invalid genesis Metadosis storage layout: {0}")]
    InvalidMetadosisStorageLayout(String),
    #[error(
        "fresh-devnet Metadosis storage layout hash mismatch: manifest {manifest}, node {node}"
    )]
    MetadosisStorageLayoutHashMismatch { manifest: B256, node: B256 },
}

impl OcompForkInstallError {
    #[must_use]
    pub fn invalid_genesis_config(error: impl core::fmt::Display) -> Self {
        Self::InvalidGenesisConfig(error.to_string())
    }

    #[must_use]
    pub fn invalid_canonical_install(error: impl core::fmt::Display) -> Self {
        Self::InvalidCanonicalInstall(error.to_string())
    }

    #[must_use]
    pub fn chain_binding_mismatch(error: impl core::fmt::Display) -> Self {
        Self::ChainBindingMismatch(error.to_string())
    }

    #[must_use]
    pub fn hash_install(error: impl core::fmt::Display) -> Self {
        Self::HashInstall(error.to_string())
    }

    #[must_use]
    pub fn invalid_genesis_field(key: &'static str, error: impl core::fmt::Display) -> Self {
        Self::InvalidGenesisField {
            key,
            reason: error.to_string(),
        }
    }

    #[must_use]
    pub fn invalid_metadosis_storage_layout(error: impl core::fmt::Display) -> Self {
        Self::InvalidMetadosisStorageLayout(error.to_string())
    }
}

impl LocalRegistryAdmissionError {
    #[must_use]
    pub fn replacement_authorization(error: impl core::fmt::Display) -> Self {
        Self::ReplacementAuthorization(error.to_string())
    }
}

impl ExternalRegistryAdmissionError {
    fn map_reason(ctor: fn(String) -> Self, error: impl core::fmt::Display) -> Self {
        ctor(error.to_string())
    }

    #[must_use]
    pub fn slot_plan(error: impl core::fmt::Display) -> Self {
        Self::map_reason(Self::SlotPlan, error)
    }

    #[must_use]
    pub fn proof(error: impl core::fmt::Display) -> Self {
        Self::map_reason(Self::Proof, error)
    }

    #[must_use]
    pub fn registry(error: impl core::fmt::Display) -> Self {
        Self::map_reason(Self::Registry, error)
    }
}

impl HistoricalCommitteeError {
    #[must_use]
    pub fn from_display(error: impl core::fmt::Display) -> Self {
        Self::new(error.to_string())
    }
}

/// Typed validation errors when authenticating historical committee proofs.
#[derive(Debug, thiserror::Error)]
pub enum HistoricalCommitteeValidationError {
    #[error("historical committee account proof rejected: {0}")]
    AccountProofRejected(String),
    #[error(
        "historical committee storage proof count mismatch: expected {expected}, got {actual}"
    )]
    StorageProofCountMismatch { expected: usize, actual: usize },
    #[error("historical committee storage proof {index} rejected: {reason}")]
    StorageProofRejected { index: usize, reason: String },
}

impl HistoricalCommitteeValidationError {
    #[must_use]
    pub fn account_proof_rejected(error: impl core::fmt::Display) -> Self {
        Self::AccountProofRejected(error.to_string())
    }

    #[must_use]
    pub fn storage_proof_rejected(index: usize, error: impl core::fmt::Display) -> Self {
        Self::StorageProofRejected {
            index,
            reason: error.to_string(),
        }
    }
}

impl From<HistoricalCommitteeValidationError> for HistoricalCommitteeError {
    fn from(error: HistoricalCommitteeValidationError) -> Self {
        Self::from_display(error)
    }
}

impl FinalizedIntentVerifierError {
    #[must_use]
    pub fn header_decode(error: impl core::fmt::Display) -> Self {
        Self::HeaderDecode(error.to_string())
    }

    #[must_use]
    pub fn consensus_finality(error: impl core::fmt::Display) -> Self {
        Self::ConsensusFinality(error.to_string())
    }

    #[must_use]
    pub fn account_proof(error: impl core::fmt::Display) -> Self {
        Self::AccountProof(error.to_string())
    }

    #[must_use]
    pub fn storage_proof(index: usize, error: impl core::fmt::Display) -> Self {
        Self::StorageProof {
            index,
            reason: error.to_string(),
        }
    }

    #[must_use]
    pub fn job_record_encoding(error: impl core::fmt::Display) -> Self {
        Self::JobRecordEncoding(error.to_string())
    }
}

impl FinalizedIntentProofBuildError {
    #[must_use]
    pub fn state(error: impl core::fmt::Display) -> Self {
        Self::State(error.to_string())
    }

    #[must_use]
    pub fn intent(error: impl core::fmt::Display) -> Self {
        Self::Intent(error.to_string())
    }
}

/// Typed state-access errors when building a finalized OCOMP intent proof.
#[derive(Debug, thiserror::Error)]
pub enum FinalizedIntentProofStateError {
    #[error("open block {block_number} {block_hash}: {reason}")]
    OpenBlockState {
        block_number: u64,
        block_hash: B256,
        reason: String,
    },
    #[error("read raw opening slot {slot}: {reason}")]
    ReadRawOpeningSlot { slot: B256, reason: String },
}

impl FinalizedIntentProofStateError {
    #[must_use]
    pub fn open_block_state(
        block_number: u64,
        block_hash: B256,
        error: impl core::fmt::Display,
    ) -> Self {
        Self::OpenBlockState {
            block_number,
            block_hash,
            reason: error.to_string(),
        }
    }

    #[must_use]
    pub fn read_raw_opening_slot(slot: B256, error: impl core::fmt::Display) -> Self {
        Self::ReadRawOpeningSlot {
            slot,
            reason: error.to_string(),
        }
    }
}

impl From<FinalizedIntentProofStateError> for FinalizedIntentProofBuildError {
    fn from(error: FinalizedIntentProofStateError) -> Self {
        Self::State(error.to_string())
    }
}

/// Typed trie/witness errors when building a finalized OCOMP intent proof.
#[derive(Debug, thiserror::Error)]
pub enum FinalizedIntentProofTrieError {
    #[error("build Metadosis intent proof: {0}")]
    BuildMetadosisIntentProof(String),
    #[error("build ValidatorSet committee proof: {0}")]
    BuildValidatorSetCommitteeProof(String),
    #[error("build raw opening proof for {contract_address}: {reason}")]
    BuildRawOpeningProof {
        contract_address: Address,
        reason: String,
    },
    #[error("account proof opened no account")]
    AccountProofOpenedNoAccount,
    #[error("storage proof count exceeds u32")]
    StorageProofCountExceedsU32,
    #[error("proof node count exceeds u32")]
    ProofNodeCountExceedsU32,
    #[error("proof node length exceeds u32")]
    ProofNodeLengthExceedsU32,
    #[error("committee length exceeds u32")]
    CommitteeLengthExceedsU32,
    #[error("VRF group key length exceeds u32")]
    VrfGroupKeyLengthExceedsU32,
}

impl FinalizedIntentProofTrieError {
    #[must_use]
    pub fn build_metadosis_intent_proof(error: impl core::fmt::Display) -> Self {
        Self::BuildMetadosisIntentProof(error.to_string())
    }

    #[must_use]
    pub fn build_validator_set_committee_proof(error: impl core::fmt::Display) -> Self {
        Self::BuildValidatorSetCommitteeProof(error.to_string())
    }

    #[must_use]
    pub fn build_raw_opening_proof(
        contract_address: Address,
        error: impl core::fmt::Display,
    ) -> Self {
        Self::BuildRawOpeningProof {
            contract_address,
            reason: error.to_string(),
        }
    }
}

impl From<FinalizedIntentProofTrieError> for FinalizedIntentProofBuildError {
    fn from(error: FinalizedIntentProofTrieError) -> Self {
        Self::Trie(error.to_string())
    }
}

/// Typed intent-source errors when building a finalized OCOMP intent proof.
#[derive(Debug, thiserror::Error)]
pub enum FinalizedIntentProofIntentError {
    #[error("invalid StorageBytes length word")]
    InvalidStorageBytesLengthWord,
    #[error("StorageBytes length exceeds usize")]
    StorageBytesLengthExceedsUsize,
}

impl From<FinalizedIntentProofIntentError> for FinalizedIntentProofBuildError {
    fn from(error: FinalizedIntentProofIntentError) -> Self {
        Self::Intent(error.to_string())
    }
}

/// Typed finalization-record errors when building a finalized OCOMP intent proof.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum FinalizedIntentProofFinalizationError {
    #[error("certified notarization cannot authorize an OCOMP JobId")]
    CertifiedNotarizationCannotAuthorizeJobId,
    #[error("historical committee length exceeds usize")]
    HistoricalCommitteeLengthExceedsUsize,
    #[error("VRF material version exceeds u64")]
    VrfMaterialVersionExceedsU64,
    #[error("VRF group key length exceeds usize")]
    VrfGroupKeyLengthExceedsUsize,
}

impl From<FinalizedIntentProofFinalizationError> for FinalizedIntentProofBuildError {
    fn from(error: FinalizedIntentProofFinalizationError) -> Self {
        match error {
            FinalizedIntentProofFinalizationError::CertifiedNotarizationCannotAuthorizeJobId => {
                Self::Finalization("certified notarization cannot authorize an OCOMP JobId")
            }
            FinalizedIntentProofFinalizationError::HistoricalCommitteeLengthExceedsUsize => {
                Self::Finalization("historical committee length exceeds usize")
            }
            FinalizedIntentProofFinalizationError::VrfMaterialVersionExceedsU64 => {
                Self::Finalization("VRF material version exceeds u64")
            }
            FinalizedIntentProofFinalizationError::VrfGroupKeyLengthExceedsUsize => {
                Self::Finalization("VRF group key length exceeds usize")
            }
        }
    }
}

impl PublicFinalizedIntentProofBuildError {
    #[must_use]
    pub fn finalization(error: impl core::fmt::Display) -> Self {
        Self::Finalization(error.to_string())
    }

    #[must_use]
    pub fn account_proof(error: impl core::fmt::Display) -> Self {
        Self::AccountProof(error.to_string())
    }

    #[must_use]
    pub fn storage_proof(error: impl core::fmt::Display) -> Self {
        Self::StorageProof(error.to_string())
    }

    #[must_use]
    pub fn intent(error: impl core::fmt::Display) -> Self {
        Self::Intent(error.to_string())
    }

    #[must_use]
    pub fn witness(error: impl core::fmt::Display) -> Self {
        Self::Witness(error.to_string())
    }
}

impl RetentionError {
    #[must_use]
    pub fn source(error: impl core::fmt::Display) -> Self {
        Self::Source(error.to_string())
    }
}

/// Typed source/provider errors wrapped by [`RetentionError::Source`].
#[derive(Debug, thiserror::Error)]
pub enum RetentionSourceError {
    #[error("open exact block state for Lysis openings: {0}")]
    OpenBlockStateForLysisOpenings(String),
    #[error("derive opening JobId: {0}")]
    DeriveOpeningJobId(String),
    #[error("read Oracle pair index: {0}")]
    ReadOraclePairIndex(String),
    #[error("Oracle pair index does not fit u32")]
    OraclePairIndexDoesNotFitU32,
    #[error("derive finalized request JobId: {0}")]
    DeriveFinalizedRequestJobId(String),
    #[error("decode finalized intent: {0}")]
    DecodeFinalizedIntent(String),
    #[error("derive proof IntentId: {0}")]
    DeriveProofIntentId(String),
    #[error("derive proof JobId: {0}")]
    DeriveProofJobId(String),
    #[error("decode finalized OCOMP request: {0}")]
    DecodeFinalizedOcompRequest(String),
    #[error("open exact block state: {0}")]
    OpenExactBlockState(String),
    #[error("derive intent slot: {0}")]
    DeriveIntentSlot(String),
    #[error("decode typed job record: {0}")]
    DecodeTypedJobRecord(String),
    #[error("hash typed job record: {0}")]
    HashTypedJobRecord(String),
    #[error("load finalized opening header: {0}")]
    LoadFinalizedOpeningHeader(String),
    #[error("finalized opening header is unavailable")]
    FinalizedOpeningHeaderUnavailable,
    #[error("validate canonical finalized OCOMP job: {0}")]
    ValidateCanonicalFinalizedOcompJob(String),
    #[error("terminal Job is missing terminal record")]
    TerminalJobMissingTerminalRecord,
    #[error("hash activation preconditions: {0}")]
    HashActivationPreconditions(String),
    #[error("read job record base slot: {0}")]
    ReadJobRecordBaseSlot(String),
    #[error("invalid StorageBytes length word")]
    InvalidStorageBytesLengthWord,
    #[error("read job record data slot {index}: {reason}")]
    ReadJobRecordDataSlot { index: usize, reason: String },
}

impl RetentionSourceError {
    #[must_use]
    pub fn open_block_state_for_lysis_openings(error: impl core::fmt::Display) -> Self {
        Self::OpenBlockStateForLysisOpenings(error.to_string())
    }

    #[must_use]
    pub fn derive_opening_job_id(error: impl core::fmt::Display) -> Self {
        Self::DeriveOpeningJobId(error.to_string())
    }

    #[must_use]
    pub fn read_oracle_pair_index(error: impl core::fmt::Display) -> Self {
        Self::ReadOraclePairIndex(error.to_string())
    }

    #[must_use]
    pub fn derive_finalized_request_job_id(error: impl core::fmt::Display) -> Self {
        Self::DeriveFinalizedRequestJobId(error.to_string())
    }

    #[must_use]
    pub fn decode_finalized_intent(error: impl core::fmt::Display) -> Self {
        Self::DecodeFinalizedIntent(error.to_string())
    }

    #[must_use]
    pub fn derive_proof_intent_id(error: impl core::fmt::Display) -> Self {
        Self::DeriveProofIntentId(error.to_string())
    }

    #[must_use]
    pub fn derive_proof_job_id(error: impl core::fmt::Display) -> Self {
        Self::DeriveProofJobId(error.to_string())
    }

    #[must_use]
    pub fn decode_finalized_ocomp_request(error: impl core::fmt::Display) -> Self {
        Self::DecodeFinalizedOcompRequest(error.to_string())
    }

    #[must_use]
    pub fn open_exact_block_state(error: impl core::fmt::Display) -> Self {
        Self::OpenExactBlockState(error.to_string())
    }

    #[must_use]
    pub fn derive_intent_slot(error: impl core::fmt::Display) -> Self {
        Self::DeriveIntentSlot(error.to_string())
    }

    #[must_use]
    pub fn decode_typed_job_record(error: impl core::fmt::Display) -> Self {
        Self::DecodeTypedJobRecord(error.to_string())
    }

    #[must_use]
    pub fn hash_typed_job_record(error: impl core::fmt::Display) -> Self {
        Self::HashTypedJobRecord(error.to_string())
    }

    #[must_use]
    pub fn load_finalized_opening_header(error: impl core::fmt::Display) -> Self {
        Self::LoadFinalizedOpeningHeader(error.to_string())
    }

    #[must_use]
    pub fn validate_canonical_finalized_ocomp_job(error: impl core::fmt::Display) -> Self {
        Self::ValidateCanonicalFinalizedOcompJob(error.to_string())
    }

    #[must_use]
    pub fn hash_activation_preconditions(error: impl core::fmt::Display) -> Self {
        Self::HashActivationPreconditions(error.to_string())
    }

    #[must_use]
    pub fn read_job_record_base_slot(error: impl core::fmt::Display) -> Self {
        Self::ReadJobRecordBaseSlot(error.to_string())
    }

    #[must_use]
    pub fn read_job_record_data_slot(index: usize, error: impl core::fmt::Display) -> Self {
        Self::ReadJobRecordDataSlot {
            index,
            reason: error.to_string(),
        }
    }
}

impl From<RetentionSourceError> for RetentionError {
    fn from(error: RetentionSourceError) -> Self {
        Self::Source(error.to_string())
    }
}

/// Typed malformed-journal errors wrapped by [`RetentionError::MalformedJournal`].
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum RetentionMalformedJournalError {
    #[error("journal exceeds byte cap")]
    JournalExceedsByteCap,
    #[error("checksum mismatch")]
    ChecksumMismatch,
    #[error("wrong magic")]
    WrongMagic,
    #[error("offset overflow")]
    OffsetOverflow,
    #[error("truncated field")]
    TruncatedField,
    #[error("field length")]
    FieldLength,
    #[error("truncated pin record")]
    TruncatedPinRecord,
    #[error("trailing bytes")]
    TrailingBytes,
    #[error("unknown state tag")]
    UnknownStateTag,
    #[error("registry version length")]
    RegistryVersionLength,
}

impl From<RetentionMalformedJournalError> for RetentionError {
    fn from(error: RetentionMalformedJournalError) -> Self {
        match error {
            RetentionMalformedJournalError::JournalExceedsByteCap => {
                Self::MalformedJournal("journal exceeds byte cap")
            }
            RetentionMalformedJournalError::ChecksumMismatch => {
                Self::MalformedJournal("checksum mismatch")
            }
            RetentionMalformedJournalError::WrongMagic => Self::MalformedJournal("wrong magic"),
            RetentionMalformedJournalError::OffsetOverflow => {
                Self::MalformedJournal("offset overflow")
            }
            RetentionMalformedJournalError::TruncatedField => {
                Self::MalformedJournal("truncated field")
            }
            RetentionMalformedJournalError::FieldLength => Self::MalformedJournal("field length"),
            RetentionMalformedJournalError::TruncatedPinRecord => {
                Self::MalformedJournal("truncated pin record")
            }
            RetentionMalformedJournalError::TrailingBytes => {
                Self::MalformedJournal("trailing bytes")
            }
            RetentionMalformedJournalError::UnknownStateTag => {
                Self::MalformedJournal("unknown state tag")
            }
            RetentionMalformedJournalError::RegistryVersionLength => {
                Self::MalformedJournal("registry version length")
            }
        }
    }
}

/// Typed transition errors wrapped by [`RetentionError::InvalidTransition`].
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum RetentionTransitionError {
    #[error("GC claim changed before completion")]
    GcClaimChangedBeforeCompletion,
    #[error("OCOMP projection/GC fence is poisoned")]
    ProjectionGcFencePoisoned,
}

impl From<RetentionTransitionError> for RetentionError {
    fn from(error: RetentionTransitionError) -> Self {
        match error {
            RetentionTransitionError::GcClaimChangedBeforeCompletion => {
                Self::InvalidTransition("GC claim changed before completion")
            }
            RetentionTransitionError::ProjectionGcFencePoisoned => {
                Self::InvalidTransition("OCOMP projection/GC fence is poisoned")
            }
        }
    }
}
