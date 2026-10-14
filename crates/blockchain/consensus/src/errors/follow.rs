//! Follower consensus errors for `outbe-consensus`.

use std::fmt;

/// Typed errors for the follower engine and upstream seam.
#[derive(Debug, thiserror::Error)]
pub enum FollowError {
    #[error("local boundary is not a decodable full DKG output")]
    LocalBoundaryNotDecodable,

    #[error("boundary outcome is not a decodable full DKG output: {0}")]
    BoundaryOutcomeDecode(#[source] crate::dkg_manager::OdkoDecodeError),

    #[error("boundary outcome epoch label {label} does not match registered epoch {registered}")]
    BoundaryOutcomeEpochMismatch { label: u64, registered: u64 },

    #[error("conflicting committee outcome replay for epoch {epoch}")]
    ConflictingCommitteeOutcomeReplay { epoch: u64 },

    #[error("committee epoch {epoch} is not sequential after authenticated epoch {highest}")]
    NonSequentialCommitteeEpoch { epoch: u64, highest: u64 },

    #[error("first registered committee epoch {epoch} is not anchor epoch {anchor}")]
    FirstRegisteredEpochNotAnchor { epoch: u64, anchor: u64 },

    #[error(
        "anchor mismatch: start-epoch {epoch} committee ({committee_len} validators) does not match the \
         trusted anchor validator set ({anchor_len} validators)"
    )]
    AnchorMismatch {
        epoch: u64,
        committee_len: usize,
        anchor_len: usize,
    },

    #[error("failed to build committee verifier for epoch {epoch}")]
    BuildCommitteeVerifier { epoch: u64 },

    #[error("failed to decode block artifacts: {0}")]
    DecodeBlockArtifacts(String),

    #[error("no committee verifier registered for epoch {epoch}")]
    NoCommitteeVerifier { epoch: u64 },

    #[error("finalization certificate failed verification for epoch {epoch}")]
    FinalizationVerificationFailed { epoch: u64 },

    #[error("certified epoch {certified} is neither routed epoch {routed} nor its successor")]
    CertifiedEpochNotRoutedOrSuccessor { certified: u64, routed: u64 },

    #[error("failed to decode authenticated block artifacts: {0}")]
    DecodeAuthenticatedBlockArtifacts(String),

    #[error("epoch changes from {from} to {to} without its BoundaryOutcome")]
    EpochChangedWithoutBoundaryOutcome { from: u64, to: u64 },

    #[error("authenticated epoch {certified} block pre-announces non-successor epoch {announced}")]
    PreAnnouncesNonSuccessor { certified: u64, announced: u64 },

    #[error("boundary artifact epoch {boundary} differs from certificate epoch {certificate}")]
    BoundaryEpochDiffersFromCertificate { boundary: u64, certificate: u64 },

    #[error("boundary outcome conflicts with authenticated epoch {epoch} outcome: {reason}")]
    BoundaryOutcomeConflicts { epoch: u64, reason: String },

    #[error("committee transition does not announce the immediate successor")]
    TransitionNotImmediateSuccessor,

    #[error("committee transition lacks a pre-announce")]
    TransitionLacksPreAnnounce,

    #[error("epoch {target} carrier is finalized by non-predecessor epoch {certified}")]
    CarrierFinalizedByNonPredecessor { target: u64, certified: u64 },

    #[error("anchor epoch {epoch} is already established")]
    AnchorAlreadyEstablished { epoch: u64 },

    #[error(
        "anchor epoch {epoch}'s first block carried no boundary outcome; cannot establish the trust root"
    )]
    AnchorMissingBoundaryOutcome { epoch: u64 },

    #[error("follower marshal exited: {0}")]
    MarshalExited(String),

    #[error("follower shutdown signal failed: {0}")]
    ShutdownSignalFailed(String),

    #[error("follower marshal exited unexpectedly")]
    MarshalExitedUnexpectedly,

    #[error("upstream did not return recovered finalized height {height}")]
    MissingRecoveredFinalizedHeight { height: u64 },

    #[error("recovered epoch {recovered} precedes follower anchor epoch {anchor}")]
    RecoveredEpochPrecedesAnchor { recovered: u64, anchor: u64 },

    #[error("reconstructed follower epoch does not contain recovered height {height}")]
    ReconstructedEpochDoesNotContainRecoveredHeight { height: u64 },

    #[error("certified block reports height {actual}, expected {expected}")]
    CertifiedBlockHeightMismatch { actual: u64, expected: u64 },

    #[error("finalization payload differs from block at height {height}")]
    FinalizationPayloadMismatch { height: u64 },

    #[error("height {height} is outside the observed follower epoch window")]
    HeightOutsideEpochWindow { height: u64 },

    #[error("authenticated delivery at height {height} rejected: {reason}")]
    AuthenticatedDeliveryRejected { height: u64, reason: String },

    #[error("authenticated ancestor at height {height} rejected: {reason}")]
    AuthenticatedAncestorRejected { height: u64, reason: String },

    #[error("missing observed activation for prior epoch {epoch}")]
    MissingPriorEpochActivation { epoch: u64 },

    #[error(
        "recovered height {recovered} precedes activation window {earliest}..={latest} for epoch {epoch}"
    )]
    RecoveredHeightPrecedesActivationWindow {
        recovered: u64,
        earliest: u64,
        latest: u64,
        epoch: u64,
    },

    #[error("failed to decode boundary candidate at {height}: {reason}")]
    DecodeBoundaryCandidate { height: u64, reason: String },

    #[error(
        "epoch {epoch} boundary candidate at height {height} is finalized by epoch {cert_epoch}"
    )]
    BoundaryCandidateWrongEpoch {
        epoch: u64,
        height: u64,
        cert_epoch: u64,
    },

    #[error("no BoundaryOutcome for epoch {epoch} in authenticated search window {earliest}..={scan_end}")]
    MissingBoundaryOutcomeInWindow {
        epoch: u64,
        earliest: u64,
        scan_end: u64,
    },

    #[error("missing prior activation height")]
    MissingPriorActivationHeight,

    #[error(
        "no prior-epoch-authenticated CommitteePreAnnounce for epoch {epoch} before boundary {boundary_height}"
    )]
    MissingCommitteePreAnnounce { epoch: u64, boundary_height: u64 },

    #[error("epoch {epoch} boundary block did not confirm its committee")]
    BoundaryDidNotConfirmCommittee { epoch: u64 },

    #[error("epocher has no activation carrier for anchor epoch")]
    NoAnchorActivationCarrier,

    #[error(
        "upstream did not return the anchor epoch's boundary block at height {height}; \
         cannot bootstrap the committee chain"
    )]
    MissingAnchorBoundaryBlock { height: u64 },

    #[error("anchor carrier registered epoch {registered}, expected {expected}")]
    AnchorCarrierEpochMismatch { registered: u64, expected: u64 },

    #[error("follower replay suffix lower height {lower} exceeds upper height {upper}")]
    ReplayLowerExceedsUpper { lower: u64, upper: u64 },

    #[error("upstream did not return follower replay suffix height {height}")]
    MissingReplaySuffixHeight { height: u64 },

    #[error("failed to decode retained follower block {height} artifacts: {reason}")]
    DecodeRetainedBlockArtifacts { height: u64, reason: String },

    #[error("failed to inspect follower replay block at height {height}: {reason}")]
    InspectReplayBlock { height: u64, reason: String },

    #[error("upstream did not return retained follower history height {height}")]
    MissingRetainedHistoryHeight { height: u64 },

    #[error("upstream did not return retained follower preannounce height {height}")]
    MissingRetainedPreannounceHeight { height: u64 },

    #[error(
        "local retained follower preannounce differs from authenticated upstream at height {height}"
    )]
    RetainedPreannounceDiffers { height: u64 },

    #[error("failed to read follower replay finalization at height {height}: {reason}")]
    ReadReplayFinalization { height: u64, reason: String },

    #[error(
        "local follower replay finalization proposal differs from authenticated upstream at height {height}"
    )]
    ReplayFinalizationProposalDiffers { height: u64 },

    #[error(
        "local follower replay finalization certificate failed verification at height {height}: {reason}"
    )]
    ReplayFinalizationVerificationFailed { height: u64, reason: String },

    #[error("failed to repair follower replay finalization at height {height}: {reason}")]
    RepairReplayFinalization { height: u64, reason: String },

    #[error("read replay ancestor certificate: {0}")]
    ReadReplayAncestorCertificate(String),

    #[error("local replay ancestor certificate payload mismatch")]
    ReplayAncestorCertificatePayloadMismatch,

    #[error("read follower replay block: {0}")]
    ReadReplayBlock(String),

    #[error("local follower replay block differs from authenticated upstream at height {height}")]
    ReplayBlockDiffers { height: u64 },

    #[error("repair follower replay block: {0}")]
    RepairReplayBlock(String),

    #[error("failed to sync repaired follower replay finalizations: {0}")]
    SyncReplayFinalizations(String),

    #[error("failed to sync repaired follower replay blocks: {0}")]
    SyncReplayBlocks(String),

    #[error("too many finality ancestors")]
    TooManyFinalityAncestors,

    #[error("invalid finality ancestor chain")]
    InvalidFinalityAncestorChain,
}

impl FollowError {
    pub fn decode_block_artifacts(err: impl fmt::Debug) -> Self {
        Self::DecodeBlockArtifacts(format!("{err:?}"))
    }

    pub fn decode_authenticated_block_artifacts(err: impl fmt::Debug) -> Self {
        Self::DecodeAuthenticatedBlockArtifacts(format!("{err:?}"))
    }

    pub fn boundary_outcome_conflicts(epoch: u64, err: impl fmt::Display) -> Self {
        Self::BoundaryOutcomeConflicts {
            epoch,
            reason: err.to_string(),
        }
    }

    pub fn marshal_exited(err: impl fmt::Debug) -> Self {
        Self::MarshalExited(format!("{err:?}"))
    }

    pub fn shutdown_signal_failed(err: impl fmt::Debug) -> Self {
        Self::ShutdownSignalFailed(format!("{err:?}"))
    }

    pub fn authenticated_delivery_rejected(height: u64, err: impl fmt::Display) -> Self {
        Self::AuthenticatedDeliveryRejected {
            height,
            reason: err.to_string(),
        }
    }

    pub fn authenticated_ancestor_rejected(height: u64, err: impl fmt::Display) -> Self {
        Self::AuthenticatedAncestorRejected {
            height,
            reason: err.to_string(),
        }
    }

    pub fn decode_boundary_candidate(height: u64, err: impl fmt::Debug) -> Self {
        Self::DecodeBoundaryCandidate {
            height,
            reason: format!("{err:?}"),
        }
    }

    pub fn decode_retained_block_artifacts(height: u64, err: impl fmt::Debug) -> Self {
        Self::DecodeRetainedBlockArtifacts {
            height,
            reason: format!("{err:?}"),
        }
    }

    pub fn inspect_replay_block(height: u64, err: impl fmt::Display) -> Self {
        Self::InspectReplayBlock {
            height,
            reason: err.to_string(),
        }
    }

    pub fn read_replay_finalization(height: u64, err: impl fmt::Display) -> Self {
        Self::ReadReplayFinalization {
            height,
            reason: err.to_string(),
        }
    }

    pub fn replay_finalization_verification_failed(height: u64, err: impl fmt::Display) -> Self {
        Self::ReplayFinalizationVerificationFailed {
            height,
            reason: err.to_string(),
        }
    }

    pub fn repair_replay_finalization(height: u64, err: impl fmt::Display) -> Self {
        Self::RepairReplayFinalization {
            height,
            reason: err.to_string(),
        }
    }

    pub fn read_replay_ancestor_certificate(err: impl fmt::Display) -> Self {
        Self::ReadReplayAncestorCertificate(err.to_string())
    }

    pub fn read_replay_block(err: impl fmt::Display) -> Self {
        Self::ReadReplayBlock(err.to_string())
    }

    pub fn repair_replay_block(err: impl fmt::Display) -> Self {
        Self::RepairReplayBlock(err.to_string())
    }

    pub fn sync_replay_finalizations(err: impl fmt::Display) -> Self {
        Self::SyncReplayFinalizations(err.to_string())
    }

    pub fn sync_replay_blocks(err: impl fmt::Display) -> Self {
        Self::SyncReplayBlocks(err.to_string())
    }
}
