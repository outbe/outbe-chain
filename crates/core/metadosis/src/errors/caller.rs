use outbe_ocomp_protocol::ProtocolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisCallerError {
    #[error("invalid OCOMP request limit receipt: {error}")]
    InvalidOcompRequestLimitReceipt { error: ProtocolError },

    #[error("formed OCOMP day limit cannot be replaced")]
    FormedOcompDayLimitCannotBeReplaced,

    #[error("unknown WorldwideDay status")]
    UnknownWorldwideDayStatus,

    #[error("formed OCOMP day limit is immutable")]
    FormedOcompDayLimitImmutable,

    #[error("Metadosis forming window overflow")]
    FormingWindowOverflow,

    #[error("Metadosis lookback window overflow")]
    LookbackWindowOverflow,

    #[error("Metadosis offering window overflow")]
    OfferingWindowOverflow,

    #[error("Metadosis waiting window overflow")]
    WaitingWindowOverflow,

    #[error("Metadosis {label} duration overflow")]
    DurationOverflow { label: &'static str },

    #[error("Metadosis UTC+14 timestamp overflow")]
    UtcPlus14TimestampOverflow,

    #[error("OCOMP result vote has no open response window")]
    ResultVoteHasNoOpenResponseWindow,

    #[error("OCOMP result vote requires an open or quorum-certified job")]
    ResultVoteRequiresOpenOrQuorumCertifiedJob,

    #[error("OCOMP result vote does not match pinned job binding")]
    ResultVotePinnedJobBindingMismatch,

    #[error("OCOMP result vote historical snapshot is missing")]
    ResultVoteHistoricalSnapshotMissing,

    #[error("OCOMP result vote member is missing")]
    ResultVoteMemberMissing,

    #[error("invalid OCOMP result vote: {error}")]
    InvalidOcompResultVote { error: ProtocolError },

    #[error("invalid OCOMP vote transition: {error}")]
    InvalidOcompVoteTransition { error: ProtocolError },
}

impl From<MetadosisCallerError> for PrecompileError {
    fn from(value: MetadosisCallerError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResultVoteCarrierInvalidError {
    #[error("{error}")]
    Preflight { error: PrecompileError },

    #[error("malformed canonical result vote: {error}")]
    MalformedCanonicalResultVote { error: ProtocolError },

    #[error("invalid result vote: {error}")]
    InvalidResultVote { error: ProtocolError },

    #[error("result vote job is not finalized")]
    JobNotFinalized,

    #[error("result vote does not match the pinned job binding")]
    PinnedJobBindingMismatch,

    #[error("result vote member is not in the pinned snapshot")]
    MemberNotInPinnedSnapshot,

    #[error("result vote carrier signer is not authorized")]
    CarrierSignerNotAuthorized,

    #[error("result vote has no response window")]
    NoResponseWindow,

    #[error("result vote has no open response window")]
    NoOpenResponseWindow,

    #[error("result vote does not match the closed response window")]
    ClosedResponseWindowMismatch,

    #[error("closed-window vote member is not in the snapshot")]
    ClosedWindowMemberNotInSnapshot,

    #[error("closed-window carrier signer is not authorized")]
    ClosedWindowCarrierSignerNotAuthorized,
}
