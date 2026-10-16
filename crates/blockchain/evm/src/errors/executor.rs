//! Typed errors for the Outbe block executor, block builder, and system transaction builder.

use super::TeeAttestationActivationError;
use alloy_evm::block::{BlockExecutionError, BlockValidationError};
use alloy_primitives::{Address, B256};
use core::fmt::{Debug, Display};
use outbe_primitives::{
    error::PrecompileError, reshare_artifact::ExecutionSummaryArtifact, system_tx::SystemTxKind,
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
            | ExecutorStorageError::MissingAccountedParentExecutionSummary { .. } => {
                Self::BodyReadUnavailable(error.to_string())
            }
            ExecutorStorageError::HookFlush(_)
            | ExecutorStorageError::LoadPrecompileAccount { .. }
            | ExecutorStorageError::SponsoredQuotaWrite(_)
            | ExecutorStorageError::SponsoredQuotaFlush(_)
            | ExecutorStorageError::FreeTxSignerCodeLookup(_) => Self::Storage(error.to_string()),
        }
    }
}

/// Internal errors that occur when the proposer constructs or signs system transactions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SystemTxBuildError {
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
    InvalidBlock1Ost3BootstrapPayload(String),
    #[error("invalid mandatory TEE ChainSpec authority: {0}")]
    InvalidMandatoryTeeChainSpecAuthority(String),
    #[error(transparent)]
    ActiveTeePolicy(#[from] TeeAttestationActivationError),
    #[error("block-1 OST3 policy does not match the immutable ChainSpec schedule")]
    Block1Ost3PolicyScheduleMismatch,
    #[error("OST3 bootstrap payload is forbidden at block {block_number}")]
    Ost3BootstrapForbiddenAtBlock { block_number: u64 },
    #[error("encode system tx input: {0}")]
    EncodeSystemTxInput(String),
    #[error("encode terminal system tx input: {0}")]
    EncodeTerminalSystemTxInput(String),
    #[error("plan visible system tx gas: {0}")]
    PlanVisibleSystemTxGas(String),
    #[error("visible gas plan missing system tx ordinal {ordinal}")]
    VisibleGasPlanMissingOrdinal { ordinal: usize },
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
    #[error("missing EVM signer for proposer terminal system tx")]
    MissingProposerTerminalSystemTxSigner,
    #[error("terminal system tx ordinal {ordinal} exceeds u8 range")]
    TerminalSystemTxOrdinalExceedsU8 { ordinal: usize },
    #[error("build terminal system tx: {0}")]
    BuildTerminalSystemTx(String),
    #[error("sign terminal system tx: {0}")]
    SignTerminalSystemTx(String),
    #[error("missing EVM signer for proposer Phase 1 prebuild")]
    MissingPhase1PrebuildSigner,
    #[error("missing parent consensus metadata for Phase 1 prebuild")]
    MissingPhase1PrebuildMetadata,
    #[error(
        "Phase 1 prebuild: metadata hash must match block parent: expected {expected}, got {actual}"
    )]
    Phase1PrebuildMetadataHashMismatch { expected: B256, actual: B256 },
    #[error("Phase 1 prebuild: encode SystemTxInputV2: {0}")]
    Phase1PrebuildEncodeInput(String),
    #[error("Phase 1 prebuild: build unsigned tx: {0}")]
    Phase1PrebuildBuildUnsignedTx(String),
    #[error("Phase 1 prebuild: sign tx: {0}")]
    Phase1PrebuildSignTx(String),
}

impl SystemTxBuildError {
    pub(crate) fn invalid_block1_ost3_bootstrap_payload(error: impl Display) -> Self {
        Self::InvalidBlock1Ost3BootstrapPayload(error.to_string())
    }

    pub(crate) fn invalid_mandatory_tee_chain_spec_authority(error: impl Display) -> Self {
        Self::InvalidMandatoryTeeChainSpecAuthority(error.to_string())
    }

    pub(crate) fn encode_system_tx_input(error: impl Display) -> Self {
        Self::EncodeSystemTxInput(error.to_string())
    }

    pub(crate) fn encode_terminal_system_tx_input(error: impl Display) -> Self {
        Self::EncodeTerminalSystemTxInput(error.to_string())
    }

    pub(crate) fn plan_visible_system_tx_gas(error: impl Display) -> Self {
        Self::PlanVisibleSystemTxGas(error.to_string())
    }

    pub(crate) fn build_unsigned_system_tx(error: impl Display) -> Self {
        Self::BuildUnsignedSystemTx(error.to_string())
    }

    pub(crate) fn sign_system_tx(error: impl Display) -> Self {
        Self::SignSystemTx(error.to_string())
    }

    pub(crate) fn build_terminal_system_tx(error: impl Display) -> Self {
        Self::BuildTerminalSystemTx(error.to_string())
    }

    pub(crate) fn sign_terminal_system_tx(error: impl Display) -> Self {
        Self::SignTerminalSystemTx(error.to_string())
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExecutorValidationError {
    #[error("{0}")]
    BlockArtifacts(String),
    #[error("reserved system transaction cannot execute without commit")]
    ReservedSystemTxWithoutCommit,
    #[error("transaction follows the terminal OCOMP system transaction")]
    TransactionFollowsTerminalOcompSystemTx,
    #[error("active OCOMP block is missing its terminal system transaction")]
    MissingActiveOcompTerminalSystemTx,
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
    #[error("HookEvents visible gas overflow")]
    HookEventsVisibleGasOverflow,
    #[error("system tx visible gas overflow after additional charge")]
    SystemTxVisibleGasOverflowAfterAdditionalCharge,
    #[error(
        "system tx receipt gas exceeds signed gas limit: visible={visible}, signed_limit={signed_limit}, visible_base={visible_base}, additional={additional}"
    )]
    SystemTxReceiptGasExceedsSignedLimit {
        visible: u64,
        signed_limit: u64,
        visible_base: u64,
        additional: u64,
    },
    #[error("system tx cumulative visible gas overflow")]
    SystemTxCumulativeVisibleGasOverflow,
    #[error(
        "system tx visible gas exceeds block gas limit: cumulative={cumulative}, block_limit={block_limit}, visible_base={visible_base}, additional={additional}"
    )]
    SystemTxVisibleGasExceedsBlockLimit {
        cumulative: u64,
        block_limit: u64,
        visible_base: u64,
        additional: u64,
    },
    #[error("system tx failure visible gas overflow")]
    SystemTxFailureVisibleGasOverflow,
    #[error("system tx visible gas overflow")]
    SystemTxVisibleGasOverflow,
    #[error("internal system-work gas overflow")]
    InternalSystemWorkGasOverflow,
    #[error(
        "internal system-work budget exceeded: cumulative={cumulative}, limit={limit}, additional={additional}"
    )]
    InternalSystemWorkBudgetExceeded {
        cumulative: u64,
        limit: u64,
        additional: u64,
    },
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
    #[error("Phase 1 commit pre-exec: expected CertifiedParentAccounting, got {kind:?}")]
    Phase1CommitUnexpectedKind { kind: SystemTxKind },
    #[error("Phase 1 commit pre-exec: encode SystemTxInputV2: {0}")]
    Phase1CommitEncodeInput(String),
    #[error(
        "critical system tx CertifiedParentAccounting did not succeed (revert/halt) in Phase 1 pre-exec commit: {result}"
    )]
    Phase1CommitDidNotSucceed { result: String },
    #[error("missing Phase 1 system tx for block {block_number} in pre-exec verifier")]
    Phase1VerifierMissingSystemTx { block_number: u64 },
    #[error("Phase 1 pre-exec verifier expected CertifiedParentAccounting, got {kind:?}")]
    Phase1VerifierUnexpectedKind { kind: SystemTxKind },
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
        expected: Vec<SystemTxKind>,
    },
    #[error("expected {expected_kind:?} system tx at ordinal {ordinal}")]
    ExpectedSystemTxAtOrdinal {
        expected_kind: SystemTxKind,
        ordinal: usize,
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
    #[error("encode terminal system tx for visible gas plan: {0}")]
    EncodeTerminalSystemTxForVisibleGasPlan(String),
    #[error("plan visible system tx gas: {0}")]
    PlanVisibleSystemTxGas(String),
    #[error("visible gas plan missing intrinsic gas for body_index={body_index}")]
    VisibleGasPlanMissingIntrinsicGas { body_index: usize },
    #[error("visible gas plan missing protocol precharge for body_index={body_index}")]
    VisibleGasPlanMissingProtocolPrecharge { body_index: usize },
    #[error("visible base gas overflow for body_index={body_index}")]
    VisibleBaseGasOverflow { body_index: usize },
    #[error("visible gas plan missing gas limit for body_index={body_index}")]
    VisibleGasPlanMissingGasLimit { body_index: usize },
    #[error("tx to reserved system transaction address after begin-zone system txs are consumed")]
    ReservedSystemTxAfterBeginZoneConsumed,
    #[error(
        "system tx cursor/body mismatch at body_index={body_index}: cursor expects {expected_kind:?}, body has {resolved_kind:?}"
    )]
    SystemTxCursorBodyMismatch {
        body_index: usize,
        expected_kind: SystemTxKind,
        resolved_kind: SystemTxKind,
    },
    #[error(
        "critical system tx {expected_phase:?} did not succeed (revert/halt) at body_index={body_index}, block_number={block_number}, failure_code={code}: {result}"
    )]
    CriticalSystemTxDidNotSucceed {
        expected_phase: SystemTxKind,
        body_index: usize,
        block_number: u64,
        code: u16,
        result: String,
    },
    #[error("Phase 1 body[0] witness signature_hash mismatch: expected {expected}, got {actual}")]
    Phase1WitnessSignatureHashMismatch { expected: B256, actual: B256 },
    #[error(
        "system tx signer mismatch at body_index={body_index} for {expected_phase:?}: expected proposer {proposer}, got {signer}"
    )]
    SystemTxSignerMismatch {
        body_index: usize,
        expected_phase: SystemTxKind,
        proposer: Address,
        signer: Address,
    },
    #[error("system tx body_index {body_index} exceeds u8 range")]
    SystemTxBodyIndexExceedsU8 { body_index: usize },
    #[error("build expected system tx at body_index={body_index}: {reason}")]
    BuildExpectedSystemTx { body_index: usize, reason: String },
    #[error("system tx signature_hash mismatch at body_index={body_index} for {expected_phase:?}")]
    SystemTxSignatureHashMismatch {
        body_index: usize,
        expected_phase: SystemTxKind,
    },
    #[error("invalid OCOMP system carrier: {0}")]
    InvalidOcompSystemCarrier(String),
    #[error("OCOMP system carrier is not active for this block")]
    OcompSystemCarrierNotActive,
    #[error("OCOMP system carrier authorization failed: {0}")]
    OcompSystemCarrierAuthorizationFailed(String),
    #[error("OCOMP system carrier signer is not authorized")]
    OcompSystemCarrierSignerNotAuthorized,
    #[error("OCOMP system carrier execution did not succeed: {result}")]
    OcompSystemCarrierDidNotSucceed { result: String },
    #[error("decode system tx at body_index={body_index}: {reason}")]
    DecodeSystemTxAtBodyIndex { body_index: usize, reason: String },
    #[error(
        "system tx phase mismatch at body_index={body_index}: expected {expected_phase:?}, got {actual_phase:?}"
    )]
    SystemTxPhaseMismatch {
        body_index: usize,
        expected_phase: SystemTxKind,
        actual_phase: SystemTxKind,
    },
    #[error("system tx calldata mismatch at body_index={body_index} for {expected_phase:?}")]
    SystemTxCalldataMismatch {
        body_index: usize,
        expected_phase: SystemTxKind,
    },
    #[error(
        "system tx signed gas below visible base at body_index={body_index}: signed={signed}, visible_base={visible_base}"
    )]
    SystemTxSignedGasBelowVisibleBase {
        body_index: usize,
        signed: u64,
        visible_base: u64,
    },
    #[error("terminal system tx ordinal {ordinal} exceeds u8 range")]
    TerminalSystemTxOrdinalExceedsU8 { ordinal: usize },
    #[error("build expected terminal system tx: {0}")]
    BuildExpectedTerminalSystemTx(String),
    #[error("terminal system tx signature hash mismatch")]
    TerminalSystemTxSignatureHashMismatch,
    #[error("terminal system tx signer mismatch: expected proposer {proposer}, got {signer}")]
    TerminalSystemTxSignerMismatch { proposer: Address, signer: Address },
    #[error("terminal system tx intrinsic gas: {0}")]
    TerminalSystemTxIntrinsicGas(String),
    #[error("critical terminal system tx did not succeed at block {block_number}: {result}")]
    CriticalTerminalSystemTxDidNotSucceed { block_number: u64, result: String },
    #[error("terminal system transaction cannot execute without commit")]
    TerminalSystemTxWithoutCommit,
    #[error("OCOMP terminal request is not active for this block")]
    OcompTerminalRequestNotActive,
    #[error("OCOMP terminal request arrived before the begin zone completed")]
    OcompTerminalRequestBeforeBeginZoneCompleted,
    #[error("OCOMP lifecycle permits exactly one end-zone system transaction")]
    MultipleEndZoneSystemTransactions,
    #[error("terminal system transaction differs from the validated block suffix")]
    TerminalSystemTxDiffersFromValidatedSuffix,
    #[error("decode terminal system tx input: {0}")]
    DecodeTerminalSystemTxInput(String),
    #[error("end-zone system transaction is not OcompTerminalRequest")]
    EndZoneSystemTxNotOcompTerminalRequest,
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

    pub(crate) fn encode_terminal_system_tx_for_visible_gas_plan(error: impl Display) -> Self {
        Self::EncodeTerminalSystemTxForVisibleGasPlan(error.to_string())
    }

    pub(crate) fn plan_visible_system_tx_gas(error: impl Display) -> Self {
        Self::PlanVisibleSystemTxGas(error.to_string())
    }

    pub(crate) fn critical_system_tx_did_not_succeed(
        expected_phase: SystemTxKind,
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

    pub(crate) fn invalid_ocomp_system_carrier(error: impl Display) -> Self {
        Self::InvalidOcompSystemCarrier(error.to_string())
    }

    pub(crate) fn ocomp_system_carrier_authorization_failed(error: impl Display) -> Self {
        Self::OcompSystemCarrierAuthorizationFailed(error.to_string())
    }

    pub(crate) fn ocomp_system_carrier_did_not_succeed(result: impl Debug) -> Self {
        Self::OcompSystemCarrierDidNotSucceed {
            result: format!("{result:?}"),
        }
    }

    pub(crate) fn decode_system_tx_at_body_index(body_index: usize, error: impl Display) -> Self {
        Self::DecodeSystemTxAtBodyIndex {
            body_index,
            reason: error.to_string(),
        }
    }

    pub(crate) fn build_expected_terminal_system_tx(error: impl Display) -> Self {
        Self::BuildExpectedTerminalSystemTx(error.to_string())
    }

    pub(crate) fn terminal_system_tx_intrinsic_gas(error: impl Display) -> Self {
        Self::TerminalSystemTxIntrinsicGas(error.to_string())
    }

    pub(crate) fn critical_terminal_system_tx_did_not_succeed(
        block_number: u64,
        result: impl Debug,
    ) -> Self {
        Self::CriticalTerminalSystemTxDidNotSucceed {
            block_number,
            result: format!("{result:?}"),
        }
    }

    pub(crate) fn decode_terminal_system_tx_input(error: impl Display) -> Self {
        Self::DecodeTerminalSystemTxInput(error.to_string())
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
