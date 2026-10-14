//! DKG manager and actor errors for `outbe-consensus`.

use std::{fmt, path::Path, time::Duration};

use alloy_primitives::B256;

/// Typed errors for the DKG manager and DKG actor.
#[derive(Debug, thiserror::Error)]
pub enum DkgError {
    #[error("DKG ceremony requires at least one participant")]
    NoParticipants,

    #[error("no pending DKG boundary artifact")]
    NoPendingBoundaryArtifact,

    #[error("boundary artifact epoch {artifact_epoch} does not match verify epoch {verify_epoch}")]
    BoundaryEpochMismatch {
        artifact_epoch: u64,
        verify_epoch: u64,
    },

    #[error("carried DKG boundary artifact does not match local expectation")]
    BoundaryArtifactMismatch,

    #[error("no active DKG ceremony for epoch {epoch}")]
    NoActiveCeremony { epoch: u64 },

    #[error("DKG output contains a player absent from the validator set")]
    OutputPlayerAbsentFromValidatorSet,

    #[error("invalid TEE expiry exclusions: {0}")]
    InvalidTeeExpiryExclusions(String),

    #[error("DKG boundary committee snapshot build failed: {0}")]
    CommitteeSnapshotBuildFailed(String),

    #[error(
        "local DKG output does not match canonical finalized-log output ({context}): \
         local {local}, canonical {canonical}"
    )]
    CanonicalOutputMismatch {
        context: String,
        local: B256,
        canonical: B256,
    },

    #[error("failed to encode DKG boundary artifact: {0}")]
    EncodeBoundaryArtifact(String),

    #[error("trailing bytes after signed dealer log")]
    TrailingSignedDealerLogBytes,

    #[error("signed dealer log failed cryptographic verification")]
    DealerLogVerificationFailed,

    #[error("signed dealer log dealer is not in ceremony committee")]
    DealerNotInCommittee,

    #[error("{0}")]
    Reconstruct(String),

    #[error("failed to load DKG dealer retry snapshot: {0}")]
    LoadDealerRetrySnapshot(String),

    #[error("failed to stat DKG player retry snapshot: {0}")]
    StatPlayerRetrySnapshot(String),

    #[error("stored DKG player retry snapshot exceeds {max_bytes} bytes")]
    StoredPlayerRetrySnapshotTooLarge { max_bytes: u64 },

    #[error("failed to load DKG player retry snapshot: {0}")]
    LoadPlayerRetrySnapshot(String),

    #[error("failed to clear DKG dealer retry snapshot: {0}")]
    ClearDealerRetrySnapshot(String),

    #[error("failed to clear DKG player retry snapshot: {0}")]
    ClearPlayerRetrySnapshot(String),

    #[error("failed to create player: {0}")]
    CreatePlayer(String),

    #[error("persisted DKG player dealing is invalid during restart replay: {0}")]
    InvalidPersistedPlayerDealing(String),

    #[error("persisted DKG player dealing produced no ACK during restart replay")]
    PersistedPlayerDealingProducedNoAck,

    #[error("persisted DKG player dealing produced a different ACK during restart replay")]
    PersistedPlayerDealingDifferentAck,

    #[error("failed to create DKG {role} retry directory {path}: {reason}")]
    CreateRetryDirectory {
        role: String,
        path: String,
        reason: String,
    },

    #[error("failed to persist DKG {role} retry snapshot: {reason}")]
    PersistRetrySnapshot { role: String, reason: String },

    #[error("too many DKG dealer retry ACKs")]
    TooManyDealerRetryAcks,

    #[error(
        "invalid DKG dealer retry snapshot length: expected at least {expected_min}, got {actual}"
    )]
    InvalidDealerRetrySnapshotLength { expected_min: usize, actual: usize },

    #[error("invalid DKG dealer retry snapshot magic")]
    InvalidDealerRetrySnapshotMagic,

    #[error("duplicate player in DKG retry snapshot")]
    DuplicatePlayerInRetrySnapshot,

    #[error("too many DKG player retry dealings")]
    TooManyPlayerRetryDealings,

    #[error("DKG player retry snapshot exceeds {max_bytes} bytes")]
    PlayerRetrySnapshotTooLarge { max_bytes: usize },

    #[error("invalid DKG player retry snapshot length: {actual}")]
    InvalidPlayerRetrySnapshotLength { actual: usize },

    #[error("invalid DKG player retry snapshot magic")]
    InvalidPlayerRetrySnapshotMagic,

    #[error("duplicate dealer in DKG player retry snapshot")]
    DuplicateDealerInPlayerRetrySnapshot,

    #[error("DKG retry field exceeds u32 length")]
    RetryFieldExceedsU32,

    #[error("DKG retry {field} length overflow")]
    RetryFieldLengthOverflow { field: String },

    #[error("truncated DKG retry {field} length")]
    TruncatedRetryFieldLength { field: String },

    #[error("DKG retry {field} value overflow")]
    RetryFieldValueOverflow { field: String },

    #[error("truncated DKG retry {field} value")]
    TruncatedRetryFieldValue { field: String },

    #[error("invalid DKG retry {field}: {reason}")]
    InvalidRetryField { field: String, reason: String },

    #[error("trailing bytes in DKG retry {field}")]
    TrailingRetryFieldBytes { field: String },

    #[error("DKG retry {kind} count {count} exceeds validator limit")]
    RetryEntryCountExceedsValidatorLimit { kind: String, count: u32 },

    #[error("trailing bytes in DKG {role} retry snapshot")]
    TrailingRetrySnapshotBytes { role: String },

    #[error("dealer-only DKG P2P receiver error: {0}")]
    DealerOnlyP2pReceiver(String),

    #[error("dealer-only DKG called for a target-set player")]
    DealerOnlyTargetSetPlayer,

    #[error("dealer-only DKG called for a key outside the previous dealer set")]
    DealerOnlyOutsidePreviousDealerSet,

    #[error("failed to create dealer-only DKG info: {0}")]
    CreateDealerOnlyInfo(String),

    #[error("dealer-only DKG requires at least one target player")]
    DealerOnlyNoTargetPlayers,

    #[error("failed to start dealer-only DKG dealer: {0}")]
    StartDealerOnlyDealer(String),

    #[error("failed to replay durable dealer-only ACK: {0}")]
    ReplayDealerOnlyAck(String),

    #[error("dealer-only DKG timed out after {timeout:?} (acks: {acks}/{player_threshold})")]
    DealerOnlyTimedOut {
        timeout: Duration,
        acks: usize,
        player_threshold: u32,
    },

    #[error("dealer-only DKG finalized log failed verification")]
    DealerOnlyFinalizedLogVerificationFailed,

    #[error("failed to publish dealer-only DKG local dealer log")]
    PublishDealerOnlyLocalDealerLog,

    #[error("invalid signed dealer log encoding: {0}")]
    InvalidSignedDealerLogEncoding(String),

    #[error("DKG P2P receiver error: {0}")]
    P2pReceiver(String),

    #[error(
        "DKG ceremony timed out after {timeout:?} \
         (acks: {acks}/{player_threshold}, logs: {logs}/{log_target})"
    )]
    CeremonyTimedOut {
        timeout: Duration,
        acks: usize,
        player_threshold: u32,
        logs: usize,
        log_target: u32,
    },

    #[error("our own finalized log failed verification")]
    OwnFinalizedLogVerificationFailed,

    #[error("player finalize failed: {0}")]
    PlayerFinalize(String),

    #[error("failed to create DKG info: {0}")]
    CreateInfo(String),

    #[error("local dealer retry snapshot was not initialized")]
    LocalDealerRetrySnapshotNotInitialized,

    #[error("failed to start dealer: {0}")]
    StartDealer(String),

    #[error("failed to process self-ack: {0}")]
    ProcessSelfAck(String),

    #[error("failed to replay durable DKG ACK: {0}")]
    ReplayDurableAck(String),
}

impl DkgError {
    pub fn invalid_tee_expiry_exclusions(err: impl fmt::Display) -> Self {
        Self::InvalidTeeExpiryExclusions(err.to_string())
    }

    pub fn committee_snapshot_build_failed(err: impl fmt::Display) -> Self {
        Self::CommitteeSnapshotBuildFailed(err.to_string())
    }

    pub fn encode_boundary_artifact(err: impl fmt::Display) -> Self {
        Self::EncodeBoundaryArtifact(err.to_string())
    }

    pub fn reconstruct(err: impl fmt::Display) -> Self {
        Self::Reconstruct(err.to_string())
    }

    pub fn load_dealer_retry_snapshot(err: impl fmt::Display) -> Self {
        Self::LoadDealerRetrySnapshot(err.to_string())
    }

    pub fn stat_player_retry_snapshot(err: impl fmt::Display) -> Self {
        Self::StatPlayerRetrySnapshot(err.to_string())
    }

    pub fn load_player_retry_snapshot(err: impl fmt::Display) -> Self {
        Self::LoadPlayerRetrySnapshot(err.to_string())
    }

    pub fn clear_dealer_retry_snapshot(err: impl fmt::Display) -> Self {
        Self::ClearDealerRetrySnapshot(err.to_string())
    }

    pub fn clear_player_retry_snapshot(err: impl fmt::Display) -> eyre::Report {
        Self::ClearPlayerRetrySnapshot(err.to_string()).into()
    }

    pub fn create_player(err: impl fmt::Debug) -> Self {
        Self::CreatePlayer(format!("{err:?}"))
    }

    pub fn invalid_persisted_player_dealing(err: impl fmt::Debug) -> Self {
        Self::InvalidPersistedPlayerDealing(format!("{err:?}"))
    }

    pub fn create_retry_dir(role: &str, path: &Path, err: impl fmt::Display) -> Self {
        Self::CreateRetryDirectory {
            role: role.to_owned(),
            path: path.display().to_string(),
            reason: err.to_string(),
        }
    }

    pub fn persist_retry_snapshot(role: &str, err: impl fmt::Display) -> eyre::Report {
        Self::PersistRetrySnapshot {
            role: role.to_owned(),
            reason: err.to_string(),
        }
        .into()
    }

    pub fn invalid_retry_field(field: &str, err: impl fmt::Display) -> Self {
        Self::InvalidRetryField {
            field: field.to_owned(),
            reason: err.to_string(),
        }
    }

    pub fn dealer_only_p2p_receiver(err: impl fmt::Display) -> Self {
        Self::DealerOnlyP2pReceiver(err.to_string())
    }

    pub fn create_dealer_only_info(err: impl fmt::Debug) -> Self {
        Self::CreateDealerOnlyInfo(format!("{err:?}"))
    }

    pub fn start_dealer_only_dealer(err: impl fmt::Debug) -> Self {
        Self::StartDealerOnlyDealer(format!("{err:?}"))
    }

    pub fn replay_dealer_only_ack(err: impl fmt::Debug) -> Self {
        Self::ReplayDealerOnlyAck(format!("{err:?}"))
    }

    pub fn invalid_signed_dealer_log_encoding(err: impl fmt::Debug) -> Self {
        Self::InvalidSignedDealerLogEncoding(format!("{err:?}"))
    }

    pub fn p2p_receiver(err: impl fmt::Display) -> Self {
        Self::P2pReceiver(err.to_string())
    }

    pub fn player_finalize(err: impl fmt::Debug) -> Self {
        Self::PlayerFinalize(format!("{err:?}"))
    }

    pub fn create_info(err: impl fmt::Debug) -> Self {
        Self::CreateInfo(format!("{err:?}"))
    }

    pub fn start_dealer(err: impl fmt::Debug) -> Self {
        Self::StartDealer(format!("{err:?}"))
    }

    pub fn process_self_ack(err: impl fmt::Debug) -> Self {
        Self::ProcessSelfAck(format!("{err:?}"))
    }

    pub fn replay_durable_ack(err: impl fmt::Debug) -> Self {
        Self::ReplayDurableAck(format!("{err:?}"))
    }
}
