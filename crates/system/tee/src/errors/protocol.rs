//! Typed errors for finalized admission codec, upgrade key transfer, and DKG ceremony.

use thiserror::Error;

use super::TransportError;

/// Malformed field or framing reasons in [`crate::finalized_admission::FinalizedAdmissionCodecError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum FinalizedAdmissionMalformedReason {
    #[error("upgrade artifact")]
    UpgradeArtifact,
    #[error("version")]
    Version,
    #[error("registry proof count")]
    RegistryProofCount,
    #[error("empty certified header field")]
    EmptyCertifiedHeaderField,
    #[error("empty V2 ancestry")]
    EmptyV2Ancestry,
    #[error("truncated field")]
    TruncatedField,
    #[error("trailing bytes")]
    TrailingBytes,
}

impl FinalizedAdmissionMalformedReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpgradeArtifact => "upgrade artifact",
            Self::Version => "version",
            Self::RegistryProofCount => "registry proof count",
            Self::EmptyCertifiedHeaderField => "empty certified header field",
            Self::EmptyV2Ancestry => "empty V2 ancestry",
            Self::TruncatedField => "truncated field",
            Self::TrailingBytes => "trailing bytes",
        }
    }
}

/// Errors produced during upgrade key transfer validation and streaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum UpgradeTransferError {
    #[error("upgrade proof dimensions exceed limits")]
    DimensionsExceedLimits,
    #[error("invalid upgrade committee record size")]
    InvalidCommitteeRecordSize,
    #[error("proof size overflow")]
    ProofSizeOverflow,
    #[error("upgrade proof exceeds aggregate limit")]
    ExceedsAggregateLimit,
    #[error("invalid upgrade context")]
    InvalidUpgradeContext,
    #[error("offset overflow")]
    OffsetOverflow,
    #[error("chunk overflow")]
    ChunkOverflow,
    #[error("source enclave is not configured")]
    SourceEnclaveNotConfigured,
    #[error("source artifact codec")]
    SourceArtifactCodec,
    #[error("source returned a different recipient context")]
    SourceRecipientContextMismatch,
    #[error("source artifact encoding")]
    SourceArtifactEncoding,
    #[error("artifact codec")]
    ArtifactCodec,
}

impl From<UpgradeTransferError> for TransportError {
    fn from(error: UpgradeTransferError) -> Self {
        Self::EnclaveError(error.to_string())
    }
}

/// Wire decoding errors for TEE DKG gossip messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TeeDkgMalformedWireReason {
    #[error("empty payload")]
    EmptyPayload,
    #[error("truncated length")]
    TruncatedLength,
    #[error("truncated field")]
    TruncatedField,
    #[error("unknown tag")]
    UnknownTag,
    #[error("trailing bytes")]
    TrailingBytes,
}

impl TeeDkgMalformedWireReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyPayload => "empty payload",
            Self::TruncatedLength => "truncated length",
            Self::TruncatedField => "truncated field",
            Self::UnknownTag => "unknown tag",
            Self::TrailingBytes => "trailing bytes",
        }
    }
}

/// Unexpected enclave response or gossip closure phases during TEE DKG ceremony.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TeeDkgUnexpectedResponseReason {
    #[error("DkgOpen")]
    DkgOpen,
    #[error("DkgStartDealer")]
    DkgStartDealer,
    #[error("DkgPlayerIngest")]
    DkgPlayerIngest,
    #[error("DkgDealerReceiveAck")]
    DkgDealerReceiveAck,
    #[error("DkgDealerFinalize")]
    DkgDealerFinalize,
    #[error("DkgPlayerFinalize")]
    DkgPlayerFinalize,
    #[error("DkgTributeOfferPartial")]
    DkgTributeOfferPartial,
    #[error("DkgFinalizeTributeOffer")]
    DkgFinalizeTributeOffer,
    #[error("gossip closed before ceremony completed")]
    GossipClosedBeforeCeremonyCompleted,
    #[error("gossip closed before founding offer-key finalization completed")]
    GossipClosedBeforeOfferKeyFinalizationCompleted,
}

impl TeeDkgUnexpectedResponseReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DkgOpen => "DkgOpen",
            Self::DkgStartDealer => "DkgStartDealer",
            Self::DkgPlayerIngest => "DkgPlayerIngest",
            Self::DkgDealerReceiveAck => "DkgDealerReceiveAck",
            Self::DkgDealerFinalize => "DkgDealerFinalize",
            Self::DkgPlayerFinalize => "DkgPlayerFinalize",
            Self::DkgTributeOfferPartial => "DkgTributeOfferPartial",
            Self::DkgFinalizeTributeOffer => "DkgFinalizeTributeOffer",
            Self::GossipClosedBeforeCeremonyCompleted => "gossip closed before ceremony completed",
            Self::GossipClosedBeforeOfferKeyFinalizationCompleted => {
                "gossip closed before founding offer-key finalization completed"
            }
        }
    }
}
