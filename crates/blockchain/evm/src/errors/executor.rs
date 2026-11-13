//! Typed errors for the Outbe block executor, block builder, and system transaction builder.

use super::TeeAttestationActivationError;
use alloy_evm::block::{BlockExecutionError, BlockValidationError};
use alloy_primitives::{Address, B256};
use core::fmt::{Debug, Display};
use outbe_primitives::{
    error::PrecompileError, reshare_artifact::ExecutionSummaryArtifact, system_tx::SysTxSelector,
};

/// Node-local storage and body-read errors in the block executor and builder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExecutorStorageError {
    #[error("read accounted-parent artifact: {0}")]
    ReadAccountedParentArtifact(String),
    #[error(
        "missing execution summary artifact for accounted-parent block {block_number} ({block_hash})"
    )]
    MissingAccountedParentExecutionSummary { block_number: u64, block_hash: B256 },
    #[error("outbe hook flush: {0}")]
    HookFlush(String),
    #[error("load precompile account {address}: {reason}")]
    LoadPrecompileAccount { address: Address, reason: String },
    #[error("sponsored quota write failed after inclusion: {0}")]
    SponsoredQuotaWrite(String),
    #[error("sponsored quota flush failed after inclusion: {0}")]
    SponsoredQuotaFlush(String),
    #[error("free-tx signer code lookup failed: {0}")]
    FreeTxSignerCodeLookup(String),
}

impl ExecutorStorageError {
    pub(crate) fn read_accounted_parent_artifact(error: impl Display) -> PrecompileError {
        Self::ReadAccountedParentArtifact(error.to_string()).into()
    }

    pub(crate) fn missing_accounted_parent_execution_summary(
        block_number: u64,
        block_hash: B256,
    ) -> PrecompileError {
        Self::MissingAccountedParentExecutionSummary {
            block_number,
            block_hash,
        }
        .into()
    }

    pub(crate) fn hook_flush(error: impl Display) -> PrecompileError {
        Self::HookFlush(error.to_string()).into()
    }

    pub(crate) fn load_precompile_account(
        address: Address,
        error: impl Display,
    ) -> PrecompileError {
        Self::LoadPrecompileAccount {
            address,
            reason: error.to_string(),
        }
        .into()
    }

    pub(crate) fn sponsored_quota_write(error: impl Display) -> PrecompileError {
        Self::SponsoredQuotaWrite(error.to_string()).into()
    }

    pub(crate) fn sponsored_quota_flush(error: impl Display) -> PrecompileError {
        Self::SponsoredQuotaFlush(error.to_string()).into()
    }

    pub(crate) fn free_tx_signer_code_lookup(error: impl Display) -> PrecompileError {
        Self::FreeTxSignerCodeLookup(error.to_string()).into()
    }
}

impl From<ExecutorStorageError> for PrecompileError {
    fn from(error: ExecutorStorageError) -> Self {
        match error {
            ExecutorStorageError::ReadAccountedParentArtifact(_)
            | ExecutorStorageError::MissingAccountedParentExecutionSummary { .. }
            | ExecutorStorageError::HookFlush(_)
            | ExecutorStorageError::LoadPrecompileAccount { .. }
            | ExecutorStorageError::SponsoredQuotaWrite(_)
            | ExecutorStorageError::SponsoredQuotaFlush(_)
            | ExecutorStorageError::FreeTxSignerCodeLookup(_) => Self::Storage(error.to_string()),
        }
    }
}

/// Internal errors that occur when the proposer constructs or signs system transactions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SystemTxBuildError {
    #[error("missing mandatory block-1 boundary outcome")]
    MissingBlock1BoundaryOutcome,
    #[error("missing expected block-1 TEE policy")]
    MissingBlock1ExpectedTeePolicy,
    #[error("missing publication batch for active publication step")]
    MissingPublicationBatch,
    #[error("encode system tx payload: {0}")]
    PayloadEncode(#[source] PrecompileError),
    #[error("OST3 bootstrap payload is invalid at genesis")]
    Ost3BootstrapInvalidAtGenesis,
    #[error("missing EVM signer for proposer system tx")]
    MissingProposerSystemTxSigner,
    #[error("configured EVM signer {signer} does not match proposer {proposer}")]
    ConfiguredSignerMismatch { signer: Address, proposer: Address },
    #[error("missing parent consensus metadata for CertifiedParentAccounting")]
    MissingParentConsensusMetadata,
    #[error(
        "CertifiedParentAccounting metadata hash must match block parent: expected {expected}, got {actual}"
    )]
    CertifiedParentAccountingHashMismatch { expected: B256, actual: B256 },
    #[error("missing mandatory block-1 OST3 bootstrap payload")]
    MissingBlock1Ost3BootstrapPayload,
    #[error("invalid block-1 OST3 bootstrap payload: {0}")]
    InvalidBlock1Ost3BootstrapPayload(outbe_primitives::tee_attestation_v1::CodecError),
    #[error("invalid mandatory TEE ChainSpec authority: {0}")]
    InvalidMandatoryTeeChainSpecAuthority(String),
    #[error(transparent)]
    ActiveTeePolicy(#[from] TeeAttestationActivationError),
    #[error("block-1 OST3 policy does not match the immutable ChainSpec schedule")]
    Block1Ost3PolicyScheduleMismatch,
    /// The block builder has no runtime host, so it cannot check the bootstrap.
    #[error("evm runtime host is not installed")]
    RuntimeHostNotInstalled,
    #[error("OST3 bootstrap payload is forbidden at block {block_number}")]
    Ost3BootstrapForbiddenAtBlock { block_number: u64 },
    #[error("encode system tx input: {0}")]
    EncodeSystemTxInput(String),
    #[error("prebuilt Phase 1 tx calldata diverges from re-derived input")]
    PrebuiltPhase1CalldataDiverges,
    #[error("prebuilt Phase 1 signer {signer} does not match proposer {proposer}")]
    PrebuiltPhase1SignerMismatch { signer: Address, proposer: Address },
    #[error("system tx ordinal {ordinal} exceeds u8 range")]
    SystemTxOrdinalExceedsU8 { ordinal: usize },
    #[error("build unsigned system tx: {0}")]
    BuildUnsignedSystemTx(String),
    #[error("sign system tx: {0}")]
    SignSystemTx(String),
    #[error("missing EVM signer for proposer Phase 1 prebuild")]
    MissingPhase1PrebuildSigner,
    #[error("missing parent consensus metadata for Phase 1 prebuild")]
    MissingPhase1PrebuildMetadata,
    #[error(
        "Phase 1 prebuild: metadata hash must match block parent: expected {expected}, got {actual}"
    )]
    Phase1PrebuildMetadataHashMismatch { expected: B256, actual: B256 },
    #[error("Phase 1 prebuild: encode SystemTx: {0}")]
    Phase1PrebuildEncodeInput(String),
    #[error("Phase 1 prebuild: build unsigned tx: {0}")]
    Phase1PrebuildBuildUnsignedTx(String),
    #[error("Phase 1 prebuild: sign tx: {0}")]
    Phase1PrebuildSignTx(String),
}

impl SystemTxBuildError {
    pub(crate) fn invalid_mandatory_tee_chain_spec_authority(error: impl Display) -> Self {
        Self::InvalidMandatoryTeeChainSpecAuthority(error.to_string())
    }

    pub(crate) fn encode_system_tx_input(error: impl Display) -> Self {
        Self::EncodeSystemTxInput(error.to_string())
    }

    pub(crate) fn build_unsigned_system_tx(error: impl Display) -> Self {
        Self::BuildUnsignedSystemTx(error.to_string())
    }

    pub(crate) fn sign_system_tx(error: impl Display) -> Self {
        Self::SignSystemTx(error.to_string())
    }

    pub(crate) fn phase1_prebuild_encode_input(error: impl Display) -> Self {
        Self::Phase1PrebuildEncodeInput(error.to_string())
    }

    pub(crate) fn phase1_prebuild_build_unsigned_tx(error: impl Display) -> Self {
        Self::Phase1PrebuildBuildUnsignedTx(error.to_string())
    }

    pub(crate) fn phase1_prebuild_sign_tx(error: impl Display) -> Self {
        Self::Phase1PrebuildSignTx(error.to_string())
    }
}

impl From<SystemTxBuildError> for BlockExecutionError {
    fn from(error: SystemTxBuildError) -> Self {
        Self::other(error)
    }
}

/// Deterministic block validation errors emitted by the Outbe block executor and builder.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExecutorValidationError {
    #[error("missing mandatory block-1 boundary outcome")]
    MissingBlock1BoundaryOutcome,
    #[error("missing expected block-1 TEE policy")]
    MissingBlock1ExpectedTeePolicy,
    #[error("block-1 TEE policy does not match the ChainSpec schedule")]
    Block1Ost3PolicyScheduleMismatch,
    #[error("invalid block-1 TEE bootstrap payload: {0}")]
    InvalidBlock1Ost3BootstrapPayload(#[source] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("missing publication batch for active publication step")]
    MissingPublicationBatch,
    #[error("encode system tx payload: {0}")]
    PayloadEncode(#[source] PrecompileError),
    #[error("unexpected non-empty system tx payload for {module} ({len} bytes)")]
    UnexpectedNonEmptyPayload { module: &'static str, len: usize },
    #[error(transparent)]
    ParentAccounting(#[from] outbe_primitives::system_tx::binding::ParentAccountingBindingError),
    #[error(transparent)]
    LateCredits(#[from] outbe_primitives::system_tx::binding::LateCreditsBindingError),
    #[error(transparent)]
    Boundary(#[from] outbe_primitives::system_tx::binding::BoundaryBindingError),
    #[error(transparent)]
    Publication(#[from] outbe_primitives::system_tx::binding::PublicationBindingError),
    #[error("invalid TeeBootstrap payload at block {block_number}: {source}")]
    TeeBootstrap {
        block_number: u64,
        #[source]
        source: outbe_primitives::tee_attestation_v1::CodecError,
    },
    #[error(transparent)]
    SystemTx(#[from] outbe_primitives::system_tx::SystemTxError),
    #[error(transparent)]
    TimestampOverflow(#[from] outbe_primitives::errors::TimestampOverflow),
    #[error("{0}")]
    BlockArtifacts(String),
    #[error("reserved system transaction cannot execute without commit")]
    ReservedSystemTxWithoutCommit,
    #[error("consensus witness execution requires execute_transaction_with_commit_condition")]
    WitnessRequiresCommitCondition,
    #[error("missing Outbe post-execution output")]
    MissingPostExecutionOutput,
    #[error("missing execution summary artifact in block extra_data")]
    MissingExecutionSummaryArtifact,
    #[error("execution summary artifact mismatch: header={header:?}, local={local:?}")]
    ExecutionSummaryArtifactMismatch {
        header: ExecutionSummaryArtifact,
        local: ExecutionSummaryArtifact,
    },
    #[error("block context: {0}")]
    BlockContext(String),
    #[error("{0}")]
    HookPrecompile(String),
    #[error("{0}")]
    InvalidWithdrawals(String),
    #[error("non-genesis block beneficiary must be REWARDS_ADDRESS {expected}: got {actual}")]
    InvalidNonGenesisBeneficiary { expected: Address, actual: Address },
    #[error("invalid system tx layout: {0}")]
    InvalidSystemTxLayout(String),
    #[error("standard Ethereum post-execution changes already applied")]
    PostExecutionAlreadyApplied,
    #[error("missing proposer EVM address for begin-zone system txs")]
    MissingBeginZoneProposerAddress,
    #[error("validate proposer identity: {0}")]
    ValidateProposerIdentity(String),
    #[error("proposer EVM address is not an active consensus participant: {proposer}")]
    ProposerNotActiveConsensusParticipant { proposer: Address },
    #[error("LateFinalizeCredits pre-exec: fb_number {fb_number} >= block {block_number}")]
    LateFinalizeFbNumberAtOrAboveBlock { fb_number: u64, block_number: u64 },
    #[error(
        "LateFinalizeCredits pre-exec: fb_number {fb_number} outside inclusion window (distance {distance}, K={window_k}) for block {block_number}"
    )]
    LateFinalizeOutsideInclusionWindow {
        fb_number: u64,
        distance: u64,
        window_k: u64,
        block_number: u64,
    },
    #[error(
        "LateFinalizeCredits pre-exec: read committee snapshot epoch={epoch} key={snapshot_key}: {reason}"
    )]
    LateFinalizeReadCommitteeSnapshot {
        epoch: u64,
        snapshot_key: B256,
        reason: String,
    },
    #[error(
        "LateFinalizeCredits pre-exec: missing committee snapshot epoch={epoch} key={snapshot_key} for block {block_number}"
    )]
    LateFinalizeMissingCommitteeSnapshot {
        epoch: u64,
        snapshot_key: B256,
        block_number: u64,
    },
    #[error(
        "LateFinalizeCredits pre-exec: proof rejected for fb={fb_hash} at block {block_number}: {reason}"
    )]
    LateFinalizeProofRejected {
        fb_hash: B256,
        block_number: u64,
        reason: String,
    },
    #[error("transaction gas limit {gas_limit} is below intrinsic gas floor {floor}")]
    TransactionGasLimitBelowFloor { gas_limit: u64, floor: u64 },
    #[error("validator fee accumulator overflow")]
    ValidatorFeeAccumulatorOverflow,
    #[error("burned fee accumulator overflow")]
    BurnedFeeAccumulatorOverflow,
    #[error("sponsored quota write failed after inclusion: {0}")]
    SponsoredQuotaWriteValidation(String),
    #[error("free-tx signer account read failed: {0}")]
    FreeTxSignerAccountRead(String),
    #[error(
        "parent_artifact_hint mismatch: metadata=({metadata_number}, {metadata_hash}), actual parent=({parent_number}, {parent_hash})"
    )]
    ParentArtifactHintMismatch {
        metadata_number: u64,
        metadata_hash: B256,
        parent_number: u64,
        parent_hash: B256,
    },
    #[error("genesis child PREVRANDAO must be zero (no parent proof)")]
    GenesisChildPrevrandaoMustBeZero,
    #[error("Phase 1 pre-exec verifier expected CertifiedParentAccounting input")]
    Phase1VerifierExpectedCertifiedParentAccountingInput,
    #[error(
        "Phase 1 pre-exec: read committee snapshot for epoch={epoch} key={snapshot_key}: {reason}"
    )]
    Phase1ReadCommitteeSnapshot {
        epoch: u64,
        snapshot_key: B256,
        reason: String,
    },
    #[error("Phase 1 pre-exec: missing committee snapshot for epoch={epoch} key={snapshot_key}")]
    Phase1MissingCommitteeSnapshot { epoch: u64, snapshot_key: B256 },
    #[error(
        "Phase 1 pre-exec: verify_v2_proof rejected metadata for block {block_number}: {reason}"
    )]
    Phase1VerifyProofRejected { block_number: u64, reason: String },
    #[error(
        "PREVRANDAO does not match verified parent proof: expected {expected}, got {actual:?}"
    )]
    PrevrandaoMismatch {
        expected: B256,
        actual: Option<B256>,
    },
    #[error("Phase 1 commit pre-exec: missing Phase 1 system tx for block {block_number}")]
    Phase1CommitMissingSystemTx { block_number: u64 },
    #[error("Phase 1 commit pre-exec: expected CertifiedParentAccounting, got {kind}")]
    Phase1CommitUnexpectedKind { kind: &'static str },
    #[error("Phase 1 commit pre-exec: encode SystemTx: {0}")]
    Phase1CommitEncodeInput(String),
    #[error(
        "critical system tx CertifiedParentAccounting did not succeed (revert/halt) in Phase 1 pre-exec commit: {result}"
    )]
    Phase1CommitDidNotSucceed { result: String },
    #[error("missing Phase 1 system tx for block {block_number} in pre-exec verifier")]
    Phase1VerifierMissingSystemTx { block_number: u64 },
    #[error("Phase 1 pre-exec verifier expected CertifiedParentAccounting, got {kind}")]
    Phase1VerifierUnexpectedKind { kind: &'static str },
    #[error("Phase 1 commit pre-exec: invalid {witness_source} witness: {reason}")]
    Phase1CommitInvalidWitness {
        witness_source: &'static str,
        reason: String,
    },
    #[error("Phase 1 commit pre-exec: build unsigned witness: {0}")]
    Phase1CommitBuildUnsignedWitness(String),
    #[error("Phase 1 commit pre-exec: sign witness: {0}")]
    Phase1CommitSignWitness(String),
    #[error("missing expected begin system tx at ordinal {ordinal}")]
    MissingExpectedBeginSystemTx { ordinal: usize },
    #[error("decode expected begin system tx input: {0}")]
    DecodeExpectedBeginSystemTxInput(String),
    #[error("OST3 bootstrap payload is forbidden at block {block_number}")]
    Ost3BootstrapForbiddenAtBlock { block_number: u64 },
    #[error("unexpected system tx at body_index={body_index}; expected begin_block system txs {expected:?}")]
    UnexpectedBeginSystemTx {
        body_index: usize,
        expected: Vec<SysTxSelector>,
    },
    #[error("missing parent consensus metadata for CertifiedParentAccounting")]
    MissingParentConsensusMetadata,
    #[error("expected CertifiedParentAccounting system tx at ordinal 0")]
    ExpectedCertifiedParentAccountingAtOrdinal0,
    #[error(
        "CertifiedParentAccounting metadata hash must match block parent: expected {expected}, got {actual}"
    )]
    CertifiedParentAccountingHashMismatch { expected: B256, actual: B256 },
    #[error("BoundaryOutcome system tx artifact mismatch at ordinal {ordinal}")]
    BoundaryOutcomeArtifactMismatch { ordinal: usize },
    #[error("expected BoundaryOutcome system tx at ordinal {ordinal}")]
    ExpectedBoundaryOutcomeAtOrdinal { ordinal: usize },
    #[error("missing mandatory block-1 OST3 bootstrap payload")]
    MissingBlock1Ost3BootstrapPayload,
    #[error("expected mandatory OST3 system tx at ordinal {ordinal}")]
    ExpectedMandatoryOst3AtOrdinal { ordinal: usize },
    #[error("encode system tx for visible gas plan: {0}")]
    EncodeSystemTxForVisibleGasPlan(String),
    #[error("plan visible system tx gas: {0}")]
    PlanVisibleSystemTxGas(String),
    #[error("visible gas plan missing intrinsic gas for body_index={body_index}")]
    VisibleGasPlanMissingIntrinsicGas { body_index: usize },
    #[error("tx to reserved system transaction address after begin-zone system txs are consumed")]
    ReservedSystemTxAfterBeginZoneConsumed,
    #[error(
        "end-block {kind} gas limit {actual} does not match required {expected} at capacity {capacity}"
    )]
    EndBlockGasLimitMismatch {
        kind: &'static str,
        expected: u64,
        actual: u64,
        capacity: u64,
    },
    #[error(
        "system tx cursor/body mismatch at body_index={body_index}: cursor expects {expected_kind:?}, body has {resolved_kind:?}"
    )]
    SystemTxCursorBodyMismatch {
        body_index: usize,
        expected_kind: SysTxSelector,
        resolved_kind: SysTxSelector,
    },
    #[error(
        "critical system tx {expected_phase} did not succeed (revert/halt) at body_index={body_index}, block_number={block_number}, failure_code={code}: {result}"
    )]
    CriticalSystemTxDidNotSucceed {
        expected_phase: &'static str,
        body_index: usize,
        block_number: u64,
        code: u16,
        result: String,
    },
    #[error("Phase 1 body[0] witness signature_hash mismatch: expected {expected}, got {actual}")]
    Phase1WitnessSignatureHashMismatch { expected: B256, actual: B256 },
    #[error(
        "system tx signer mismatch at body_index={body_index} for {expected_phase}: expected proposer {proposer}, got {signer}"
    )]
    SystemTxSignerMismatch {
        body_index: usize,
        expected_phase: &'static str,
        proposer: Address,
        signer: Address,
    },
    #[error("system tx body_index {body_index} exceeds u8 range")]
    SystemTxBodyIndexExceedsU8 { body_index: usize },
    #[error("build expected system tx at body_index={body_index}: {reason}")]
    BuildExpectedSystemTx { body_index: usize, reason: String },
    #[error("system tx signature_hash mismatch at body_index={body_index} for {expected_phase}")]
    SystemTxSignatureHashMismatch {
        body_index: usize,
        expected_phase: &'static str,
    },
    #[error("decode system tx at body_index={body_index}: {reason}")]
    DecodeSystemTxAtBodyIndex { body_index: usize, reason: String },
    #[error(
        "system tx phase mismatch at body_index={body_index}: expected {expected_phase}, got {actual_phase}"
    )]
    SystemTxPhaseMismatch {
        body_index: usize,
        expected_phase: &'static str,
        actual_phase: &'static str,
    },
    #[error("system tx calldata mismatch at body_index={body_index} for {expected_phase}")]
    SystemTxCalldataMismatch {
        body_index: usize,
        expected_phase: &'static str,
    },
}

impl ExecutorValidationError {
    pub(crate) fn block_artifacts(error: impl Display) -> Self {
        Self::BlockArtifacts(error.to_string())
    }

    pub(crate) fn block_context(error: impl Display) -> Self {
        Self::BlockContext(error.to_string())
    }

    pub(crate) fn hook_precompile(error: impl Display) -> Self {
        Self::HookPrecompile(error.to_string())
    }

    pub(crate) fn invalid_withdrawals(error: impl Display) -> Self {
        Self::InvalidWithdrawals(error.to_string())
    }

    pub(crate) fn invalid_system_tx_layout(error: impl Display) -> Self {
        Self::InvalidSystemTxLayout(error.to_string())
    }

    pub(crate) fn validate_proposer_identity(error: impl Display) -> Self {
        Self::ValidateProposerIdentity(error.to_string())
    }

    pub(crate) fn late_finalize_read_committee_snapshot(
        epoch: u64,
        snapshot_key: B256,
        error: impl Display,
    ) -> Self {
        Self::LateFinalizeReadCommitteeSnapshot {
            epoch,
            snapshot_key,
            reason: error.to_string(),
        }
    }

    pub(crate) fn late_finalize_proof_rejected(
        fb_hash: B256,
        block_number: u64,
        error: impl Display,
    ) -> Self {
        Self::LateFinalizeProofRejected {
            fb_hash,
            block_number,
            reason: error.to_string(),
        }
    }

    pub(crate) fn sponsored_quota_write_validation(error: impl Display) -> Self {
        Self::SponsoredQuotaWriteValidation(error.to_string())
    }

    pub(crate) fn free_tx_signer_account_read(error: impl Display) -> Self {
        Self::FreeTxSignerAccountRead(error.to_string())
    }

    pub(crate) fn phase1_read_committee_snapshot(
        epoch: u64,
        snapshot_key: B256,
        error: impl Display,
    ) -> Self {
        Self::Phase1ReadCommitteeSnapshot {
            epoch,
            snapshot_key,
            reason: error.to_string(),
        }
    }

    pub(crate) fn phase1_verify_proof_rejected(block_number: u64, error: impl Display) -> Self {
        Self::Phase1VerifyProofRejected {
            block_number,
            reason: error.to_string(),
        }
    }

    pub(crate) fn phase1_commit_encode_input(error: impl Display) -> Self {
        Self::Phase1CommitEncodeInput(error.to_string())
    }

    pub(crate) fn phase1_commit_did_not_succeed(result: impl Debug) -> Self {
        Self::Phase1CommitDidNotSucceed {
            result: format!("{result:?}"),
        }
    }

    pub(crate) fn phase1_commit_invalid_witness(
        witness_source: &'static str,
        error: impl Display,
    ) -> Self {
        Self::Phase1CommitInvalidWitness {
            witness_source,
            reason: error.to_string(),
        }
    }

    pub(crate) fn phase1_commit_build_unsigned_witness(error: impl Display) -> Self {
        Self::Phase1CommitBuildUnsignedWitness(error.to_string())
    }

    pub(crate) fn phase1_commit_sign_witness(error: impl Display) -> Self {
        Self::Phase1CommitSignWitness(error.to_string())
    }

    pub(crate) fn decode_expected_begin_system_tx_input(error: impl Display) -> Self {
        Self::DecodeExpectedBeginSystemTxInput(error.to_string())
    }

    pub(crate) fn encode_system_tx_for_visible_gas_plan(error: impl Display) -> Self {
        Self::EncodeSystemTxForVisibleGasPlan(error.to_string())
    }

    pub(crate) fn plan_visible_system_tx_gas(error: impl Display) -> Self {
        Self::PlanVisibleSystemTxGas(error.to_string())
    }

    pub(crate) fn critical_system_tx_did_not_succeed(
        expected_phase: &'static str,
        body_index: usize,
        block_number: u64,
        code: u16,
        result: impl Debug,
    ) -> Self {
        Self::CriticalSystemTxDidNotSucceed {
            expected_phase,
            body_index,
            block_number,
            code,
            result: format!("{result:?}"),
        }
    }

    pub(crate) fn build_expected_system_tx(body_index: usize, error: impl Display) -> Self {
        Self::BuildExpectedSystemTx {
            body_index,
            reason: error.to_string(),
        }
    }

    pub(crate) fn decode_system_tx_at_body_index(body_index: usize, error: impl Display) -> Self {
        Self::DecodeSystemTxAtBodyIndex {
            body_index,
            reason: error.to_string(),
        }
    }
}

impl From<ExecutorValidationError> for BlockValidationError {
    fn from(error: ExecutorValidationError) -> Self {
        Self::msg(error.to_string())
    }
}

impl From<ExecutorValidationError> for BlockExecutionError {
    fn from(error: ExecutorValidationError) -> Self {
        Self::Validation(BlockValidationError::msg(error.to_string()))
    }
}

/// Typed error returned when settling accumulated block burned fees through the runtime host.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BurnedFeeSettlementError {
    /// Deterministic validation failure during burned-fee settlement.
    #[error("burned fee settlement validation failed: {0}")]
    Validation(#[source] PrecompileError),
    /// Node-local storage or fatal precompile error during burned-fee settlement.
    #[error("burned fee settlement internal failure: {0}")]
    Internal(#[source] PrecompileError),
    /// Storage flush failure after burned-fee settlement.
    #[error("burned fee settlement flush failed: {0}")]
    Flush(#[source] PrecompileError),
}

impl From<BurnedFeeSettlementError> for BlockExecutionError {
    fn from(error: BurnedFeeSettlementError) -> Self {
        match error {
            BurnedFeeSettlementError::Validation(_) => {
                Self::Validation(BlockValidationError::other(error))
            }
            BurnedFeeSettlementError::Internal(_) | BurnedFeeSettlementError::Flush(_) => {
                Self::other(error)
            }
        }
    }
}

// Both EL error boundaries preserve the same typed payload failures.
macro_rules! impl_payload_build_error {
    ($target:ty) => {
        impl From<outbe_primitives::system_tx_context::SystemTxPayloadBuildError> for $target {
            fn from(error: outbe_primitives::system_tx_context::SystemTxPayloadBuildError) -> Self {
                use outbe_primitives::system_tx_context::SystemTxPayloadBuildError as E;
                match error {
                    E::MissingParentConsensusMetadata { .. } => {
                        Self::MissingParentConsensusMetadata
                    }
                    E::CertifiedParentAccountingHashMismatch {
                        parent_hash,
                        metadata_hash,
                    } => Self::CertifiedParentAccountingHashMismatch {
                        expected: parent_hash,
                        actual: metadata_hash,
                    },
                    E::MissingBlock1BoundaryOutcome => Self::MissingBlock1BoundaryOutcome,
                    E::MissingBlock1TeeBootstrapPayload => Self::MissingBlock1Ost3BootstrapPayload,
                    E::MissingBlock1ExpectedTeePolicy => Self::MissingBlock1ExpectedTeePolicy,
                    E::Block1TeeBootstrapPolicyScheduleMismatch => {
                        Self::Block1Ost3PolicyScheduleMismatch
                    }
                    E::InvalidBlock1TeeBootstrapPayload(source) => {
                        Self::InvalidBlock1Ost3BootstrapPayload(source)
                    }
                    E::TeeBootstrapForbiddenAtBlock { block_number } => {
                        Self::Ost3BootstrapForbiddenAtBlock { block_number }
                    }
                    E::MissingPublicationBatch => Self::MissingPublicationBatch,
                    E::Encode(source) => Self::PayloadEncode(source),
                }
            }
        }
    };
}
impl_payload_build_error!(SystemTxBuildError);
impl_payload_build_error!(ExecutorValidationError);

impl From<outbe_primitives::system_tx_context::SystemTxPayloadValidationError>
    for ExecutorValidationError
{
    fn from(error: outbe_primitives::system_tx_context::SystemTxPayloadValidationError) -> Self {
        use outbe_primitives::system_tx_context::SystemTxPayloadValidationError as E;
        match error {
            E::UnexpectedNonEmptyPayload { module, len } => {
                Self::UnexpectedNonEmptyPayload { module, len }
            }
            E::ParentAccounting(source) => Self::ParentAccounting(source),
            E::LateCredits(source) => Self::LateCredits(source),
            E::Boundary(source) => Self::Boundary(source),
            E::Publication(source) => Self::Publication(source),
            E::TeeBootstrap {
                block_number,
                source,
            } => Self::TeeBootstrap {
                block_number,
                source,
            },
        }
    }
}

#[cfg(test)]
mod payload_conversion_tests {
    use super::*;
    use outbe_primitives::{
        system_tx::binding::ParentAccountingBindingError,
        system_tx_context::{SystemTxPayloadBuildError, SystemTxPayloadValidationError},
    };

    #[test]
    fn payload_conversions_keep_typed_causes_at_both_el_boundaries() {
        let build = SystemTxBuildError::from(SystemTxPayloadBuildError::Encode(
            PrecompileError::WriteProtection,
        ));
        assert!(matches!(
            build,
            SystemTxBuildError::PayloadEncode(PrecompileError::WriteProtection)
        ));
        let validation = ExecutorValidationError::from(SystemTxPayloadBuildError::Encode(
            PrecompileError::WriteProtection,
        ));
        assert!(matches!(
            validation,
            ExecutorValidationError::PayloadEncode(PrecompileError::WriteProtection)
        ));
        let validation =
            ExecutorValidationError::from(SystemTxPayloadValidationError::ParentAccounting(
                ParentAccountingBindingError::PayloadDecode(PrecompileError::WriteProtection),
            ));
        assert!(matches!(
            validation,
            ExecutorValidationError::ParentAccounting(ParentAccountingBindingError::PayloadDecode(
                PrecompileError::WriteProtection
            ),)
        ));
    }
}
