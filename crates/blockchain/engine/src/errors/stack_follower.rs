//! Defines error types for follower stack, startup, shutdown, and recovery operations.

use std::{fmt::Debug, time::Duration};

use alloy_primitives::B256;
use commonware_consensus::types::Height;
use outbe_primitives::projection::ProjectionCheckpoint;
use thiserror::Error;

/// Represents errors from the follower stack, local anchor restoration, and follower startup.
#[derive(Debug, Error)]
pub enum FollowerStackError {
    /// Indicates that uncertified development sync is not implemented.
    #[error("--upstream.nocertify (uncertified dev sync) is not yet implemented")]
    UpstreamNocertifyNotImplemented,
    /// Indicates that the genesis validator set is not a valid participant set.
    #[error("genesis validator set is not a valid participant set: {0}")]
    InvalidGenesisParticipantSet(String),
    /// Indicates that the selected upstream lacks the mandatory OST3 offer key.
    #[error("selected upstream has no mandatory OST3 offer key")]
    UpstreamMissingOst3OfferKey,
    /// Indicates that the local enclave offer key does not match the upstream chain offer key.
    #[error(
        "local enclave does not hold the selected chain's exact offer key; refusing certified sync (no recovery or fallback)"
    )]
    LocalEnclaveOfferKeyMismatch,
    /// Indicates that consensus storage_dir is unset before follower startup.
    #[error("consensus storage_dir must be set before follower startup")]
    MissingStorageDirBeforeStartup,
    /// Indicates that the follower finalization payload differs from the executed block.
    #[error(
        "certified follower finalization payload {finalized_hash} differs from executed block {block_hash} at height {height}"
    )]
    FinalizationPayloadDiffersFromExecuted {
        /// Finalization payload hash.
        finalized_hash: B256,
        /// Executed block hash.
        block_hash: B256,
        /// Block height.
        height: u64,
    },
    /// Indicates that the follower verifier committee differs from the historical committee snapshot.
    #[error(
        "certified follower verifier committee {verifier_hash} differs from historical committee snapshot {historical_hash} at epoch {finalized_epoch}"
    )]
    VerifierCommitteeDiffersFromHistorical {
        /// Verifier committee hash.
        verifier_hash: B256,
        /// Historical committee hash.
        historical_hash: B256,
        /// Finalized epoch number.
        finalized_epoch: u64,
    },
    /// Indicates that Marshal lacks a certified finalization at the follower height.
    #[error("marshal has no certified finalization at follower height {0}")]
    MarshalMissingCertifiedFinalization(u64),
    /// Indicates that Marshal lacks a finalized block at the follower height.
    #[error("marshal has no finalized block {payload} at follower height {height}")]
    MarshalMissingFinalizedBlock {
        /// Payload hash.
        payload: B256,
        /// Block height.
        height: u64,
    },
    /// Indicates that a Marshal finalized block reports an unexpected height.
    #[error("marshal finalized block {block_hash} reports height {actual}, expected {expected}")]
    MarshalFinalizedBlockHeightMismatch {
        /// Block hash.
        block_hash: B256,
        /// Reported block height.
        actual: u64,
        /// Expected block height.
        expected: u64,
    },
    /// Indicates that the certified follower lacks a verifier scheme for the finalized epoch.
    #[error("certified follower has no verifier scheme for finalized epoch {0}")]
    MissingVerifierSchemeForFinalizedEpoch(u64),
    /// Indicates that the certified follower lacks a historical committee snapshot for the epoch.
    #[error("certified follower has no historical committee snapshot for epoch {0}")]
    MissingHistoricalCommitteeSnapshot(u64),
    /// Indicates that reading the local floor certificate failed.
    #[error("failed to read local floor certificate {floor}: {error}")]
    ReadLocalFloorCertificate {
        /// Floor block height.
        floor: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that reading the local floor block failed.
    #[error("failed to read local floor block {floor}: {error}")]
    ReadLocalFloorBlock {
        /// Floor block height.
        floor: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that the local finalized floor certificate disagrees with canonical state.
    #[error("local finalized floor {0} certificate disagrees with canonical execution state")]
    LocalFloorCertificateDisagrees(u64),
    /// Indicates that inspecting a local follower block failed.
    #[error("failed to inspect local follower block {height}: {error}")]
    InspectLocalFollowerBlock {
        /// Block height.
        height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that a local boundary candidate has invalid block artifacts.
    #[error("invalid local boundary candidate {height}: {error}")]
    InvalidLocalBoundaryCandidate {
        /// Block height.
        height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that the canonical local block is missing at a height.
    #[error("missing canonical local block at height {0}")]
    MissingCanonicalLocalBlock(u64),
    /// Indicates that the local finalized block disagrees with canonical execution state.
    #[error("local finalized block {0} disagrees with canonical execution state")]
    LocalFinalizedBlockDisagrees(u64),
    /// Indicates that reading a local boundary certificate failed.
    #[error("failed to read local boundary certificate {height}: {error}")]
    ReadLocalBoundaryCertificate {
        /// Block height.
        height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that a local boundary certificate conflicts with the canonical block.
    #[error("local boundary certificate conflicts with canonical block {0}")]
    LocalBoundaryCertificateConflict(u64),
    /// Indicates that the local committee boundary cannot contain the floor height.
    #[error("local committee boundary {height} cannot contain floor {floor}")]
    LocalCommitteeBoundaryCannotContainFloor {
        /// Boundary block height.
        height: u64,
        /// Floor block height.
        floor: u64,
    },
    /// Indicates that reading the last block number from the provider failed.
    #[error("failed to get last block number: {0}")]
    GetLastBlockNumber(String),
    /// Indicates that replay normalization failed to pair bounded archive tips.
    #[error(
        "certified follower replay normalization did not pair bounded archive tips from height {replay_suffix_upper}: finalizations={archive_finalization_tip}, blocks={archive_block_tip}"
    )]
    UnpairedReplayArchiveTips {
        /// Replay suffix upper bound.
        replay_suffix_upper: u64,
        /// Finalization archive tip.
        archive_finalization_tip: u64,
        /// Block archive tip.
        archive_block_tip: u64,
    },
    /// Indicates that the block archive tip includes a height whose record is missing.
    #[error("follower block archive tip includes height {0} but its exact record is missing")]
    MissingArchivedFollowerBlockRecord(u64),
    /// Indicates that the recovery height changed during Marshal archive initialization.
    #[error(
        "certified follower recovery height changed while Marshal archives were initialized: inspected {inspected}, selected {selected}"
    )]
    RecoveryHeightChangedDuringMarshalInit {
        /// Inspected anchor height.
        inspected: u64,
        /// Selected recovery height.
        selected: u64,
    },
    /// Indicates that reading the canonical Reth hash at the recovery height failed.
    #[error("failed to read canonical Reth hash at recovery height {recovery_height}: {error}")]
    ReadCanonicalRethHashAtRecovery {
        /// Recovery height.
        recovery_height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that the canonical Reth block is missing at the recovery height.
    #[error("canonical Reth block is missing at recovery height {0}")]
    MissingCanonicalRethBlockAtRecovery(u64),
    /// Indicates that the local block is missing for the recovery height.
    #[error("missing local block for recovery height {0}")]
    MissingLocalBlockForRecovery(u64),
    /// Indicates that the upstream did not return an exact recovery finalization.
    #[error("upstream did not return exact recovery finalization at height {0}")]
    MissingUpstreamRecoveryFinalization(u64),
    /// Indicates that the restored follower boundary activation height is missing.
    #[error("restored follower boundary is missing")]
    MissingRestoredFollowerBoundary,
    /// Indicates that the view retention timeout multiplication overflowed.
    #[error("view retention timeout overflow")]
    ViewRetentionTimeoutOverflow,
    /// Indicates that the certified follower executor task failed.
    #[error("certified follower executor task failed: {0}")]
    ExecutorTaskFailed(String),
    /// Indicates that Marshal lost a finalized follower block before proof drain.
    #[error("marshal lost finalized follower block {0} before proof drain")]
    MarshalLostFinalizedBlockBeforeDrain(u64),
    /// Indicates that the finality observer stopped before ingress quiesced.
    #[error("certified FullNode finality observer stopped before ingress quiesced")]
    FinalityObserverStoppedBeforeQuiesce,
    /// Indicates that Marshal became unavailable before the follower proof drain completed.
    #[error("marshal unavailable before follower proof drain completed")]
    MarshalUnavailableBeforeDrainCompleted,
    /// Indicates that Marshal has not acknowledged the follower proof drain height.
    #[error(
        "marshal has not acknowledged follower proof drain: processed {processed}, persisted {persisted}"
    )]
    MarshalHasNotAcknowledgedProofDrain {
        /// Marshal processed height.
        processed: Height,
        /// Last persisted height.
        persisted: u64,
    },
}

/// Represents errors from consensus stack shutdown and engine drain.
#[derive(Debug, Error)]
pub enum StackShutdownError {
    /// Indicates that the Simplex engine failed while draining.
    #[error("simplex engine failed while draining: {0}")]
    SimplexEngineFailedWhileDraining(String),
    /// Indicates that draining the Simplex engine timed out.
    #[error("timed out after {0:?} while draining simplex engine")]
    SimplexEngineDrainTimedOut(Duration),
    /// Indicates that the terminal stack stop task failed.
    #[error("terminal stack stop task failed: {0}")]
    TerminalStackStopTaskFailed(String),
    /// Indicates that the terminal stack stop operation failed.
    #[error("terminal stack stop failed: {0}")]
    TerminalStackStopFailed(String),
    /// Indicates that both Simplex engine drain and terminal stack stop failed.
    #[error(
        "simplex engine drain failed: {engine_error}; terminal stack stop also failed: {stop_error}"
    )]
    EngineDrainAndTerminalStopFailed {
        /// Engine drain error text.
        engine_error: String,
        /// Terminal stop error text.
        stop_error: String,
    },
    /// Indicates that the Simplex engine exited with an error.
    #[error("simplex engine exited: {0}")]
    SimplexEngineExited(String),
    /// Indicates an additional failure while draining the consensus stack.
    #[error("additional failure while draining the consensus stack: {0}")]
    AdditionalDrainFailure(String),
}

impl StackShutdownError {
    /// Formats an additional failure while draining the consensus stack.
    pub fn additional_drain_failure(error: &eyre::Report) -> Self {
        Self::AdditionalDrainFailure(format!("{error:#}"))
    }
}

/// Represents errors from consensus stack startup validation.
#[derive(Debug, Error)]
pub enum StackStartupError {
    /// Indicates that genesis config uses the deprecated epochDuration field.
    #[error("genesis config uses deprecated epochDuration; use epochLengthBlocks")]
    DeprecatedEpochDuration,
    /// Indicates that genesis config uses the deprecated dkgRotationIntervalBlocks field.
    #[error("genesis config uses deprecated dkgRotationIntervalBlocks; use epochLengthBlocks")]
    DeprecatedDkgRotationIntervalBlocks,
    /// Indicates that genesis config epochLengthBlocks is zero.
    #[error("genesis config epochLengthBlocks must be > 0")]
    EpochLengthBlocksZero,
    /// Indicates that genesis config epochLengthBlocks failed deserialization.
    #[error("invalid genesis config epochLengthBlocks: {0}")]
    InvalidEpochLengthBlocks(String),
    /// Governments descriptor or the effective publication window is invalid.
    #[error("invalid governments genesis configuration: {0}")]
    InvalidGovernmentsGenesis(String),
    /// Indicates that a genesis config timing key failed deserialization.
    #[error("invalid genesis config {key}: {error}")]
    InvalidGenesisTimingConfig {
        /// Config key name.
        key: String,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that genesis config minBlockTimeMs is zero.
    #[error("genesis config minBlockTimeMs must be > 0 (the floor cannot be disabled)")]
    MinBlockTimeMsZero,
    /// Indicates that genesis config leaderTimeoutMs is zero.
    #[error("genesis config leaderTimeoutMs must be > 0")]
    LeaderTimeoutMsZero,
    /// Indicates that genesis config certificationTimeoutMs is zero.
    #[error("genesis config certificationTimeoutMs must be > 0")]
    CertificationTimeoutMsZero,
    /// Indicates that minBlockTimeMs is not less than leaderTimeoutMs.
    #[error("genesis config minBlockTimeMs ({min_ms}) must be < leaderTimeoutMs ({leader_ms})")]
    MinBlockTimeExceedsLeaderTimeout {
        /// Minimum block time in milliseconds.
        min_ms: u64,
        /// Leader timeout in milliseconds.
        leader_ms: u64,
    },
    /// Indicates that leaderTimeoutMs exceeds certificationTimeoutMs.
    #[error(
        "genesis config leaderTimeoutMs ({leader_ms}) must be <= certificationTimeoutMs ({cert_ms})"
    )]
    LeaderTimeoutExceedsCertificationTimeout {
        /// Leader timeout in milliseconds.
        leader_ms: u64,
        /// Certification timeout in milliseconds.
        cert_ms: u64,
    },
    /// Indicates that reading the genesis hash from the provider failed.
    #[error("failed to get genesis hash: {0}")]
    GetGenesisHash(String),
    /// Indicates that the genesis block hash is missing from the provider.
    #[error("missing genesis block hash from provider")]
    MissingGenesisBlockHash,
    /// Indicates that reading the genesis block from the provider failed.
    #[error("failed to read genesis block from provider: {0}")]
    ReadGenesisBlock(String),
    /// Indicates that the height-0 genesis block is missing from the provider.
    #[error("missing genesis block (height 0) from provider")]
    MissingGenesisBlock,
    /// Indicates that a required non-zero configuration value is zero.
    #[error("{0} must be > 0")]
    ValueMustBePositive(String),
    /// Indicates that a consensus peer entry lacks the `@` separator.
    #[error("invalid consensus peer {entry:?}: expected <hex_bls_pubkey>@<host:port>")]
    ConsensusPeerMissingAtSign {
        /// Raw peer entry.
        entry: String,
    },
    /// Indicates that a consensus peer entry has an empty public key.
    #[error("invalid consensus peer {entry:?}: public key is empty")]
    ConsensusPeerEmptyPublicKey {
        /// Raw peer entry.
        entry: String,
    },
    /// Indicates that a consensus peer public key is not valid hex.
    #[error("invalid consensus peer {entry:?}: public key is not hex: {error}")]
    ConsensusPeerPublicKeyNotHex {
        /// Raw peer entry.
        entry: String,
        /// Hex decode error text.
        error: String,
    },
    /// Indicates that a consensus peer decoded public key is empty.
    #[error("invalid consensus peer {entry:?}: decoded public key is empty")]
    ConsensusPeerDecodedPublicKeyEmpty {
        /// Raw peer entry.
        entry: String,
    },
    /// Indicates that a consensus peer socket address is invalid.
    #[error("invalid consensus peer {entry:?}: invalid socket address: {error}")]
    ConsensusPeerInvalidSocketAddr {
        /// Raw peer entry.
        entry: String,
        /// Socket address parse error text.
        error: String,
    },
    /// Indicates that the caller specified `--testnet.trust-el-head` on a non-test network.
    #[error("--testnet.trust-el-head is not allowed on non-test networks (chain_id {0})")]
    TrustElHeadNotAllowedOnChain(u64),
    /// Indicates that the caller specified `--testnet.unix-time-offset-secs` on a non-test network.
    #[error("--testnet.unix-time-offset-secs is not allowed on non-test networks (chain_id {0})")]
    UnixTimeOffsetNotAllowedOnChain(u64),
}

/// Represents errors from stack recovery anchor and forkchoice reconciliation.
#[derive(Debug, Error)]
pub enum StackRecoveryError {
    /// Indicates that the Marshal processed floor exceeds the recovery anchor.
    #[error(
        "Marshal processed floor {marshal_processed} exceeds certified follower recovery anchor {anchor}"
    )]
    MarshalFloorExceedsAnchor {
        /// Marshal processed floor height.
        marshal_processed: u64,
        /// Recovery anchor height.
        anchor: u64,
    },
    /// Indicates that the recovery anchor exceeds the Marshal processed floor by more than one block.
    #[error(
        "certified follower recovery anchor {anchor} exceeds Marshal processed floor {marshal_processed} by more than one block"
    )]
    AnchorExceedsMarshalFloorByMoreThanOne {
        /// Recovery anchor height.
        anchor: u64,
        /// Marshal processed floor height.
        marshal_processed: u64,
    },
    /// Indicates that the Reth finalized height exceeds the recovery anchor.
    #[error("Reth finalized height {reth_finalized} exceeds recovery anchor {anchor}")]
    RethFinalizedExceedsAnchor {
        /// Reth finalized height.
        reth_finalized: u64,
        /// Recovery anchor height.
        anchor: u64,
    },
    /// Indicates that the local recovery block differs from the canonical Reth checkpoint.
    #[error("local recovery block differs from canonical Reth checkpoint")]
    LocalBlockDiffersFromCheckpoint,
    /// Indicates that the upstream recovery ancestor differs from the local block.
    #[error("upstream recovery ancestor differs from local block")]
    UpstreamAncestorDiffersFromLocalBlock,
    /// Indicates that the recovery ancestor certificate does not match the historical committee.
    #[error("recovery ancestor certificate is not from its authenticated historical committee")]
    AncestorCertificateCommitteeMismatch,
    /// Indicates that a direct recovery certificate is missing.
    #[error("missing normalized direct recovery certificate")]
    MissingDirectRecoveryCertificate,
    /// Indicates that the recovery ancestor committee is missing.
    #[error("missing recovery ancestor committee")]
    MissingRecoveryAncestorCommittee,
    /// Indicates that the recovery descendant certificate failed verification.
    #[error("recovery descendant certificate failed verification")]
    DescendantCertificateVerificationFailed,
    /// Indicates that the local recovery certificate conflicts with the authenticated ancestor.
    #[error("local recovery certificate conflicts with authenticated ancestor")]
    LocalCertificateConflictsWithAncestor,
    /// Indicates that the local recovery ancestor certificate failed verification.
    #[error("local recovery ancestor certificate failed verification")]
    LocalAncestorCertificateVerificationFailed,
    /// Indicates that the local archived block height does not match the expected height.
    #[error("local archived block reports height {actual}, expected {expected}")]
    LocalArchivedBlockHeightMismatch {
        /// Actual block height.
        actual: u64,
        /// Expected block height.
        expected: u64,
    },
    /// Indicates that the upstream certified block height does not match the expected height.
    #[error("upstream certified block reports height {actual}, expected {expected}")]
    UpstreamCertifiedBlockHeightMismatch {
        /// Actual block height.
        actual: u64,
        /// Expected block height.
        expected: u64,
    },
    /// Indicates that the local archived block hash differs from the canonical Reth hash.
    #[error(
        "local archived block hash {actual} differs from canonical Reth hash {canonical_hash} at height {height}"
    )]
    LocalArchivedBlockHashMismatch {
        /// Actual block hash.
        actual: B256,
        /// Canonical Reth hash.
        canonical_hash: B256,
        /// Block height.
        height: u64,
    },
    /// Indicates that the upstream certified block hash differs from the canonical Reth hash.
    #[error(
        "upstream certified block hash {actual} differs from canonical Reth hash {canonical_hash} at height {height}"
    )]
    UpstreamCertifiedBlockHashMismatch {
        /// Actual block hash.
        actual: B256,
        /// Canonical Reth hash.
        canonical_hash: B256,
        /// Block height.
        height: u64,
    },
    /// Indicates that the local archived finalization payload differs from the canonical Reth hash.
    #[error(
        "local archived finalization payload {actual} differs from canonical Reth hash {canonical_hash} at height {height}"
    )]
    LocalFinalizationPayloadMismatch {
        /// Actual payload hash.
        actual: B256,
        /// Canonical Reth hash.
        canonical_hash: B256,
        /// Block height.
        height: u64,
    },
    /// Indicates that the upstream finalization payload differs from the canonical Reth hash.
    #[error(
        "upstream finalization payload {actual} differs from canonical Reth hash {canonical_hash} at height {height}"
    )]
    UpstreamFinalizationPayloadMismatch {
        /// Actual payload hash.
        actual: B256,
        /// Canonical Reth hash.
        canonical_hash: B256,
        /// Block height.
        height: u64,
    },
    /// Indicates that the local archived finalization epoch differs from the upstream epoch.
    #[error(
        "local archived finalization epoch {local_epoch} differs from authenticated upstream epoch {upstream_epoch} at height {height}"
    )]
    FinalizationEpochMismatch {
        /// Local epoch.
        local_epoch: u64,
        /// Upstream epoch.
        upstream_epoch: u64,
        /// Block height.
        height: u64,
    },
    /// Indicates that the certified follower lacks a verifier scheme for the recovery epoch.
    #[error("certified follower has no verifier scheme for recovery epoch {0}")]
    MissingRecoveryEpochVerifierScheme(u64),
    /// Indicates that the local archived finalization certificate failed verification.
    #[error("local archived finalization certificate failed verification at height {0}")]
    LocalFinalizationVerificationFailed(u64),
    /// Indicates that the upstream finalization certificate failed verification.
    #[error("upstream finalization certificate failed verification at height {0}")]
    UpstreamFinalizationVerificationFailed(u64),
    /// Indicates that Marshal returned a finalization record for the genesis height.
    #[error("marshal returned a finalization record for genesis execution height")]
    MarshalFinalizationAtGenesis,
    /// Indicates that Marshal returned no finalization record for a non-genesis height.
    #[error("marshal returned no finalization for non-genesis execution height {0}")]
    MarshalFinalizationMissingForHeight(u64),
    /// Indicates a digest mismatch between execution state and Marshal finalization.
    #[error(
        "marshal finalization digest mismatch at execution height {height}: execution={execution}, marshal={marshal}"
    )]
    MarshalFinalizationDigestMismatch {
        /// Execution height.
        height: u64,
        /// Execution block hash.
        execution: B256,
        /// Marshal digest hash.
        marshal: B256,
    },
    /// Indicates that Reth finalized identity conflicts with the recovery anchor.
    #[error(
        "Reth finalized identity {observed} conflicts with certified follower recovery anchor {anchor:?}"
    )]
    RethFinalizedIdentityConflictsWithAnchor {
        /// Formatted observed forkchoice state.
        observed: String,
        /// Recovery anchor checkpoint.
        anchor: ProjectionCheckpoint,
    },
    /// Indicates that a recovered forkchoice update failed closed.
    #[error(
        "recovered FCU failed closed for anchor {anchor:?}: outcome={outcome}, provider={observed}"
    )]
    RecoveredFcuFailedClosed {
        /// Recovery anchor checkpoint.
        anchor: ProjectionCheckpoint,
        /// Formatted attempt outcome.
        outcome: String,
        /// Formatted observed forkchoice state.
        observed: String,
    },
    /// Indicates that recovered forkchoice updates did not converge within the retry budget.
    #[error(
        "recovered FCU did not converge on anchor {anchor:?} after {attempts} attempts: last outcome={outcome}, provider={observed}"
    )]
    RecoveredFcuDidNotConverge {
        /// Recovery anchor checkpoint.
        anchor: ProjectionCheckpoint,
        /// Maximum attempts.
        attempts: usize,
        /// Formatted last outcome.
        outcome: String,
        /// Formatted observed forkchoice state.
        observed: String,
    },
    /// Indicates that Reth returned a zero canonical head hash.
    #[error("Reth returned a zero canonical head")]
    RethZeroCanonicalHead,
    /// Indicates that Marshal finalization is missing for a finalized execution height.
    #[error(
        "marshal finalization missing for finalized execution height {0}; likely partial restore/archive corruption; resync/rebuild consensus storage"
    )]
    MarshalFinalizationMissingCorrupt(u64),
    /// Indicates a timeout while recovering Marshal finalization for a finalized execution height.
    #[error(
        "timed out recovering marshal finalization for finalized execution height {0}; likely partial restore/archive corruption; resync/rebuild consensus storage"
    )]
    MarshalFinalizationTimeoutCorrupt(u64),
    /// Indicates that a recovered forkchoice update attempt timed out.
    #[error("recovered FCU attempt {0} timed out")]
    RecoveredFcuAttemptTimedOut(usize),
}
