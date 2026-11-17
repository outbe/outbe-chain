//! Secret-bearing library errors are deliberately reduced to safe operation codes.
#[derive(Debug, thiserror::Error)]
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
