//! Secret-bearing library errors are deliberately reduced to safe operation codes.
#[derive(Debug, thiserror::Error)]
/// Closed rejection reasons without secret-bearing cryptographic error dumps.
pub enum DkgError {
    #[error("DKG operation rejected: {operation:?}")]
    Operation { operation: DkgOperation },
    #[error("missing recipient encryption key")]
    MissingRecipientKey,
    #[error("participant count overflow")]
    ParticipantCountOverflow,
    #[error("empty participant set")]
    EmptyParticipantSet,
    #[error("dealer log verification failed")]
    DealerLogVerificationFailed,
    #[error("incomplete dealer set: {actual}, expected {expected}")]
    IncompleteDealerSet { actual: usize, expected: usize },
    #[error("ceremony already open")]
    CeremonyAlreadyOpen,
    #[error("ceremony missing")]
    SessionMissing,
    #[error("trailing DKG bytes")]
    TrailingBytes,
}
#[derive(Debug, thiserror::Error)]
/// Illegal lifecycle transitions for resident dealer/player state.
pub enum DkgSeamOrderError {
    #[error("dealer not started or already finalized")]
    DealerUnavailable,
    #[error("player share not finalized")]
    ShareUnavailable,
    #[error("group output not finalized")]
    OutputUnavailable,
    #[error("dealer already started")]
    StartDealerCalledTwice,
    #[error("player already finalized")]
    PlayerAlreadyFinalized,
}

#[derive(Clone, Copy, Debug)]
/// Safe operation identifiers for secret-bearing library rejections.
pub enum DkgOperation {
    DecodeOfferPartial,
    VerifyOfferPartial,
    RecoverOfferGroupSignature,
    DealerStart,
    PlayerNew,
    DecodeBlsPubkey,
    BuildParticipantSet,
    BuildCeremonyInfo,
    DecodePubMsg,
    DecodeAck,
    DecodeSignedLog,
    DecodeSealedShare,
    DealerReceiveAck,
    PlayerFinalize,
}
