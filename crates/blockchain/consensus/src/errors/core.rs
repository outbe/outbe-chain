//! Core, application, executor, and finalization errors for `outbe-consensus`.

use alloy_primitives::{Address, B256};
use commonware_consensus::types::{Epoch, Height, Round};
use outbe_primitives::{
    error::PrecompileError,
    projection::ProjectionFailureClass,
    system_tx::{
        binding::{BoundaryBindingError, LayoutBindingError, ParentAccountingBindingError},
        SystemTxError, SystemTxKind,
    },
};
use std::time::Duration;
use thiserror::Error;

use crate::digest::Digest;

/// Errors when the node stores BLS keys or bootstraps DKG material.
#[derive(Debug, Error)]
pub enum BlsKeyError {
    #[error("argon2 key derivation failed: {0}")]
    Argon2Derive(String),
    #[error("failed to create AES-256-GCM key")]
    CreateAesKey,
    #[error("AES-GCM encryption failed")]
    AesGcmEncrypt,
    #[error("keychain entry creation failed: {0}")]
    KeychainEntry(String),
    #[error("keychain store failed: {0}")]
    KeychainStore(String),
    #[error("{kind} path has no file name: {path}")]
    MissingFileName { kind: String, path: String },
    #[error("{kind} file {path} has mode {mode:o}, expected 600")]
    InsecureFileMode {
        kind: String,
        path: String,
        mode: u32,
    },
    #[error("file is encrypted but backend is not Encrypted (need passphrase)")]
    EncryptedFileRequiresPassphrase,
    #[error("encrypted file too short")]
    EncryptedFileTooShort,
    #[error("encrypted file salt slice")]
    EncryptedFileSaltSlice,
    #[error("invalid nonce length")]
    InvalidNonceLength,
    #[error("decryption failed (wrong passphrase?)")]
    DecryptionFailed,
    #[error("invalid keychain marker file")]
    InvalidKeychainMarker,
    #[error("keychain read failed (key not found?): {0}")]
    KeychainRead(String),
    #[error("keychain cleanup failed: {0}")]
    KeychainCleanup(String),
    #[error("invalid BLS12-381 signing share: {0}")]
    InvalidSigningShare(String),
    #[error("invalid BLS12-381 public polynomial: {0}")]
    InvalidPublicPolynomial(String),
    #[error("invalid BLS12-381 DKG output: {0}")]
    InvalidDkgOutput(String),
    #[error("invalid BLS12-381 individual key: {0}")]
    InvalidIndividualKey(String),
    #[error("validator count must be > 0")]
    ZeroValidatorCount,
    #[error("BLS DKG failed: {0}")]
    BlsDkgFailed(String),
    #[error("failed to compute partial public key: {0}")]
    ComputePartialPublicKey(String),
    #[error("signing share does not match public polynomial")]
    SigningSharePolynomialMismatch,
    #[error("DKG output public polynomial does not match saved public polynomial")]
    DkgOutputPolynomialMismatch,
}

/// Errors from consensus CLI subcommands.
#[derive(Debug, Error)]
pub enum ConsensusCliError {
    #[error("secret key path has no file name: {0}")]
    SecretKeyPathNoFileName(String),
    #[error("unexpected secp256k1 public key encoding for Reth enode")]
    UnexpectedSecp256k1Encoding,
    #[error("validator count must be > 0")]
    ZeroValidatorCount,
    #[error("validators.json must not be empty")]
    EmptyValidatorsJson,
    #[error("validator-{0} public_key must be 48 bytes")]
    InvalidPublicKeyLength(usize),
    #[error("validator-{0} BLS signing key does not match validators.json")]
    BlsSigningKeyMismatch(usize),
    #[error("validator-{0} EVM signing key does not match validators.json")]
    EvmSigningKeyMismatch(usize),
    #[error("too many validators for localnet Reth p2p port range")]
    TooManyLocalnetValidators,
    #[error("signing share not found at {0}")]
    SigningShareNotFound(String),
    #[error("polynomial not found at {0}")]
    PolynomialNotFound(String),
    #[error("DKG output not found at {0}")]
    DkgOutputNotFound(String),
}

/// Errors when the node registers or reacquires epoch sub-channels.
#[derive(Debug, Error)]
pub enum EpochSubchannelError {
    #[error("register {channel} subchannel for epoch {epoch}: {reason}")]
    Register {
        channel: &'static str,
        epoch: Epoch,
        reason: String,
    },
    #[error("same-epoch subchannel timeout overflows SystemTime")]
    TimeoutOverflow,
    #[error("timed out after {timeout:?} reacquiring {channel} subchannel for epoch {epoch}")]
    ReacquireTimeout {
        timeout: Duration,
        channel: &'static str,
        epoch: Epoch,
    },
    #[error("reacquire {channel} subchannel for epoch {epoch}: {reason}")]
    Reacquire {
        channel: &'static str,
        epoch: Epoch,
        reason: String,
    },
    #[error(
        "pre-registered subchannels for epoch {stash_epoch} cannot be used for current epoch {epoch}"
    )]
    StashEpochMismatch { stash_epoch: Epoch, epoch: Epoch },
}

/// Errors when the node binds or checks mainnet storage identity.
#[derive(Debug, Error)]
pub enum StorageIdentityError {
    #[error("Mainnet consensus storage {storage_dir} is nonempty but has no {marker_file} marker")]
    NonemptyWithoutMarker {
        storage_dir: String,
        marker_file: &'static str,
    },
    #[error("Mainnet consensus storage {0} must be a real directory")]
    NotRealDirectory(String),
    #[error("Mainnet consensus identity marker {0} must be a regular file")]
    MarkerNotRegularFile(String),
    #[error(
        "Mainnet consensus identity mismatch in {marker_path}: expected chain {expected_chain_id} genesis {expected_genesis_hash}, found chain {actual_chain_id} genesis {actual_genesis_hash}"
    )]
    IdentityMismatch {
        marker_path: String,
        expected_chain_id: u64,
        expected_genesis_hash: B256,
        actual_chain_id: u64,
        actual_genesis_hash: B256,
    },
}

/// Error when VRF material expires before DKG activates.
#[derive(Debug, Error)]
pub enum VrfSafetyError {
    #[error("VRF material expired: block {block_height} exceeds expiry height {expiry_height}")]
    MaterialExpired {
        block_height: u64,
        expiry_height: u64,
    },
}

/// Errors when the node checks blocks and binds system transactions.
#[derive(Debug, Error)]
pub enum BlockValidationError {
    #[error("non-genesis block beneficiary must be REWARDS_ADDRESS {expected}: got {actual}")]
    InvalidBeneficiary { expected: Address, actual: Address },
    #[error(
        "proposed block parent digest {actual} does not match Simplex context parent {expected}"
    )]
    ProposedParentDigestMismatch { actual: B256, expected: B256 },
    #[error("non-genesis Simplex context parent was not resolved for height validation")]
    UnresolvedContextParent,
    #[error("resolved parent digest {actual} does not match Simplex context parent {expected}")]
    ResolvedParentDigestMismatch { actual: B256, expected: B256 },
    #[error("parent block number overflow while validating proposal height")]
    ParentHeightOverflow,
    #[error(
        "proposed block number {actual} does not extend Simplex parent height {parent_height}"
    )]
    HeightDoesNotExtendParent { actual: u64, parent_height: u64 },
    #[error("missing certificate scheme for epoch {0}")]
    MissingCertificateScheme(Epoch),
    #[error("consensus leader public key is not in epoch {0} participant set")]
    LeaderNotInParticipantSet(Epoch),
    #[error("participant index {0} does not fit usize")]
    ParticipantIndexOverflow(u32),
    #[error("missing ordered EVM committee for epoch {0}")]
    MissingOrderedCommittee(Epoch),
    #[error("ordered EVM committee for epoch {epoch} is missing participant index {index}")]
    MissingCommitteeParticipant { epoch: Epoch, index: usize },
    #[error(
        "protocol gas limit mismatch at block {block_number}: expected {expected}, got {actual}"
    )]
    GasLimitMismatch {
        block_number: u64,
        expected: u64,
        actual: u64,
    },
    #[error("decode CertifiedParentAccounting system tx input: {0}")]
    DecodeParentAccountingInput(SystemTxError),
    #[error("{0}")]
    ParentAccountingBinding(ParentAccountingBindingError),
    #[error("decode system transaction input: {0}")]
    DecodeSystemTxInput(SystemTxError),
    #[error("{0}")]
    BoundaryBinding(BoundaryBindingError),
    #[error("{0}")]
    SystemTxEncode(#[from] SystemTxError),
    #[error("plan visible system tx gas: {0}")]
    PlanVisibleGas(SystemTxError),
    #[error("system tx ordinal {0} exceeds u8 range")]
    OrdinalOverflow(usize),
    #[error("visible gas plan missing system tx ordinal {0}")]
    MissingVisibleGasOrdinal(u8),
    #[error("build unsigned system transaction: {0}")]
    BuildUnsignedSystemTx(SystemTxError),
    #[error("system tx signature_hash mismatch for {kind:?} at ordinal {ordinal}")]
    SignatureHashMismatch { kind: SystemTxKind, ordinal: u8 },
    #[error("recover system tx signer for leader binding: {0}")]
    RecoverSigner(String),
    #[error("system tx signer {signer} does not match consensus leader EVM address {expected}")]
    SignerMismatch { signer: Address, expected: Address },
    #[error("decode Outbe block artifacts for system tx validation: {0}")]
    DecodeBlockArtifacts(PrecompileError),
    #[error("invalid system tx layout for leader binding: {0}")]
    InvalidSystemTxLayout(SystemTxError),
    #[error("{0}")]
    LayoutBinding(LayoutBindingError),
}

impl BlockValidationError {
    /// Convert a parent accounting error into a block error.
    pub fn from_parent_accounting_binding(error: ParentAccountingBindingError) -> Self {
        match error {
            ParentAccountingBindingError::Decode(source) => {
                Self::DecodeParentAccountingInput(source)
            }
            other => Self::ParentAccountingBinding(other),
        }
    }

    /// Convert a boundary outcome error into a block error.
    pub fn from_boundary_binding(error: BoundaryBindingError) -> Self {
        match error {
            BoundaryBindingError::Decode(source) => Self::DecodeSystemTxInput(source),
            other => Self::BoundaryBinding(other),
        }
    }

    /// Convert a layout error into a block error.
    pub fn from_layout_binding(error: LayoutBindingError) -> Self {
        match error {
            LayoutBindingError::Artifacts(source) => Self::DecodeBlockArtifacts(source),
            LayoutBindingError::Layout(source) => Self::InvalidSystemTxLayout(source),
            other => Self::LayoutBinding(other),
        }
    }
}

impl From<BlockValidationError> for crate::dkg_manager::ArtifactAdmissionError {
    fn from(error: BlockValidationError) -> Self {
        Self::Rejected(error.to_string())
    }
}

impl From<BlockValidationError> for String {
    fn from(error: BlockValidationError) -> Self {
        error.to_string()
    }
}

/// Error when the node decodes a header artifact from extra data.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct HeaderArtifactDecodeError(pub String);

impl From<PrecompileError> for HeaderArtifactDecodeError {
    fn from(error: PrecompileError) -> Self {
        Self(error.to_string())
    }
}

impl From<HeaderArtifactDecodeError> for String {
    fn from(error: HeaderArtifactDecodeError) -> Self {
        error.0
    }
}

impl From<HeaderArtifactDecodeError> for crate::dkg_manager::ArtifactAdmissionError {
    fn from(error: HeaderArtifactDecodeError) -> Self {
        Self::Rejected(error.0)
    }
}

impl From<HeaderArtifactDecodeError> for crate::dkg_manager::BoundaryRequirementError {
    fn from(error: HeaderArtifactDecodeError) -> Self {
        Self::Unavailable(error.0)
    }
}

/// Error when the node prepares a locally built candidate payload.
#[derive(Debug, Error)]
pub enum BuiltCandidatePreparationError {
    #[error("locally built payload was not accepted by execution: {0}")]
    Execution(String),
}

impl BuiltCandidatePreparationError {
    /// Build an execution error from any displayable error value.
    pub fn from_execution_error(error: impl std::fmt::Display) -> Self {
        Self::Execution(error.to_string())
    }
}

/// Errors when the node extracts a Phase 1 parent proof.
#[derive(Debug, Error)]
pub enum ParentProofError {
    #[error("invalid system tx layout while extracting Phase 1: {0}")]
    InvalidSystemTxLayout(SystemTxError),
    #[error("missing Phase 1 finalization system transaction for block {0}")]
    MissingPhase1SystemTx(u64),
    #[error("decode Phase 1 system transaction input: {0}")]
    DecodePhase1Input(SystemTxError),
    #[error("expected Phase 1 finalization system transaction at begin ordinal 0")]
    UnexpectedBeginOrdinal0,
}

/// Errors when the node resolves an epoch boundary parent.
#[derive(Debug, Error)]
pub enum EpochBoundaryParentError {
    /// Simplex parent does not match the committed continuity anchor.
    #[error("epoch boundary parent mismatch: simplex parent {got} != finalized anchor {expected} (epoch={epoch})")]
    ParentMismatch {
        expected: B256,
        got: B256,
        epoch: u64,
    },
    /// The finalization view has no anchor for a non-zero epoch.
    #[error("broken epoch continuity: epoch={epoch} but no finalized anchor in finalization_view")]
    MissingAnchor { epoch: u64 },
    /// Marshal store cannot return the anchor block.
    #[error("epoch boundary parent block at height {height} not found in marshal")]
    MissingMarshalBlock { height: u64 },
    /// Marshal returned a block whose digest does not match the Simplex parent.
    #[error("marshal block at height {height} has digest {got} != simplex parent {expected}")]
    MarshalHashMismatch {
        height: u64,
        expected: B256,
        got: B256,
    },
}

/// Errors from application proposal, timing, and verification handlers.
#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("refusing proposal above VRF expiry: {0}")]
    RefusingProposalAboveVrfExpiry(#[source] eyre::Report),
    #[error("system clock before UNIX_EPOCH: {0}")]
    ClockBeforeUnixEpoch(#[source] std::time::SystemTimeError),
    #[error("system clock millis does not fit in u64")]
    ClockMillisOverflow,
    #[error("Unix time offset {0} moves timestamp outside u64")]
    UnixTimeOffsetOverflow(i64),
    #[error("projection readiness failed ({class:?}): {message}")]
    ProjectionReadiness {
        class: ProjectionFailureClass,
        message: std::sync::Arc<str>,
    },
    #[error("{0}")]
    EncodeBlockArtifacts(String),
    #[error("canonicalize_and_build failed: {0}")]
    CanonicalizeAndBuild(#[source] eyre::Report),
    #[error("payload resolution returned None")]
    PayloadResolutionNone,
    #[error("payload resolution failed: {0}")]
    PayloadResolutionFailed(String),
    #[error("failed to resolve parent block {0} for proposal")]
    ResolveParentFailed(B256),
    #[error("timed out resolving parent block {0} for proposal")]
    ResolveParentTimedOut(B256),
    #[error("parent {parent} rejected by execution layer: {status}")]
    ParentRejected { parent: B256, status: String },
    #[error("new_payload failed for parent {parent}: {reason}")]
    ParentNewPayloadFailed { parent: B256, reason: String },
    #[error("failed to build block for proposal: {0}")]
    BuildBlockFailed(#[source] eyre::Report),
    #[error("DKG boundary requirement unavailable: {0}")]
    BoundaryRequirementUnavailable(crate::dkg_manager::ArtifactAdmissionError),
    #[error("candidate unavailable: {0}")]
    CandidateUnavailable(String),
    #[error("could not resolve epoch boundary parent: {0}")]
    EpochBoundaryParentUnavailable(#[source] EpochBoundaryParentError),
    #[error("parent unavailable: {0}")]
    ParentUnavailable(String),
}

/// Errors from the executor actor and its mailbox.
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("executor mailbox closed")]
    MailboxClosed,
    #[error("executor dropped response")]
    DroppedResponse,
    #[error("executor dropped finalized subscription response")]
    DroppedFinalizedSubscriptionResponse,
    #[error(
        "executor backfill cannot apply finalized block at height {height} digest {digest}: {reason}"
    )]
    BackfillApplyFailed {
        height: Height,
        digest: Digest,
        #[source]
        reason: eyre::Report,
    },
    #[error(
        "executor backfill: marshal missing finalized block at height {height} (<= reported finalized height {consensus_height}); cannot reconstruct contiguous execution state"
    )]
    BackfillMissingBlock { height: u64, consensus_height: u64 },
    #[error("FCU failed: {0}")]
    FcuFailed(String),
    #[error("FCU returned invalid: {0}")]
    FcuInvalid(String),
    #[error(
        "FCU did not return payload_id: payload_status={payload_status} latest_valid_hash={latest_valid_hash} head={head} safe={safe} finalized={finalized} head_height={head_height} finalized_height={finalized_height}"
    )]
    FcuMissingPayloadId {
        payload_status: String,
        latest_valid_hash: String,
        head: B256,
        safe: B256,
        finalized: B256,
        head_height: Height,
        finalized_height: Height,
    },
    #[error(
        "marshal delivered unexpected genesis anchor: digest {digest}, canonical finalized height {finalized_height}, canonical hash {expected}"
    )]
    UnexpectedGenesisAnchor {
        digest: Digest,
        finalized_height: Height,
        expected: B256,
    },
    #[error("executor cannot apply finalized block at height {height} digest {digest}: {reason}")]
    ApplyFinalizedBlockFailed {
        height: Height,
        digest: Digest,
        #[source]
        reason: eyre::Report,
    },
    #[error("cannot execute finalized genesis block through successor path: digest {0}")]
    ExecuteGenesisThroughSuccessor(Digest),
    #[error(
        "finalized block rejected by execution layer at height {height} digest {digest}: status={status}"
    )]
    FinalizedBlockRejected {
        height: Height,
        digest: Digest,
        status: String,
    },
    #[error(
        "failed to send finalized block to execution layer at height {height} digest {digest}: {reason}"
    )]
    SendFinalizedBlockFailed {
        height: Height,
        digest: Digest,
        reason: String,
    },
    #[error("failed to canonicalize finalized block at height {height} digest {digest}: {reason}")]
    CanonicalizeFinalizedFailed {
        height: Height,
        digest: Digest,
        #[source]
        reason: eyre::Report,
    },
    #[error(
        "executor canonicalize response dropped for finalized block at height {height} digest {digest}"
    )]
    CanonicalizeResponseDropped { height: Height, digest: Digest },
    #[error("finalized parent projection wait expired without a request budget")]
    FinalizedParentWaitExpired,
    #[error("projection is ahead of finalized parent {block_hash} at height {block_number}")]
    ProjectionAheadOfFinalizedParent { block_hash: B256, block_number: u64 },
    #[error("projection readiness failed ({class:?}): {message}")]
    ProjectionReadinessFailed {
        class: ProjectionFailureClass,
        message: std::sync::Arc<str>,
    },
    #[error("FullNode OCOMP readiness failed: {0}")]
    FullNodeOcompReadinessFailed(#[source] eyre::Report),
    #[error("consensus parent forkchoice failed: {0}")]
    ParentForkchoiceFailed(String),
    #[error("consensus parent forkchoice must be VALID: {0}")]
    ParentForkchoiceNotValid(String),
    #[error("execution cancellation does not match its cancelled request")]
    CancellationMismatch,
    #[error("unexpected cancellation of parent convergence execution")]
    UnexpectedParentConvergenceCancellation,
}

/// Errors from the finalization actor.
#[derive(Debug, Error)]
pub enum FinalizationError {
    #[error("FinalizationActor fatal: {0}")]
    ActorFatal(#[source] eyre::Report),
    #[error(
        "same-round finalization inconsistency at {round:?}: new digest {digest} conflicts with finalized hash {finalized_hash}"
    )]
    SameRoundInconsistency {
        round: Round,
        digest: Digest,
        finalized_hash: B256,
    },
    #[error("marshal mailbox required to resolve finalized block {0} not in local cache")]
    MarshalMailboxRequired(Digest),
    #[error(
        "same-height finalization inconsistency at block {block_number}: new digest {digest} conflicts with finalized hash {finalized_hash}"
    )]
    SameHeightInconsistency {
        block_number: u64,
        digest: Digest,
        finalized_hash: B256,
    },
    #[error("finalization committee snapshot build failed at epoch {finalized_epoch}: {reason}")]
    CommitteeSnapshotBuildFailed {
        finalized_epoch: u64,
        reason: String,
    },
    #[error("persist finalization parent proof record: {0}")]
    PersistParentProofRecord(String),
    #[error("prune finalization parent certificate records: {0}")]
    PruneParentCertRecords(String),
    #[error("prune temporary parent certificates: {0}")]
    PruneTemporaryParentCerts(String),
}

impl From<FinalizationError> for PrecompileError {
    fn from(error: FinalizationError) -> Self {
        Self::Fatal(error.to_string())
    }
}
