//! Typed Tribute engine failures. Display sentences stay the historical text.
//!
//! A wire `Rejected { reason }` stores [`display_text`] of the typed error.
//! The source chain is not erased.

use outbe_core_common::confidential_tee::TeeError;
use outbe_nod::nod_encryption::NodEncryptionError;
pub use outbe_nod::offer::errors::{display_text, OfferError, OfferResult, ProcessError};
use thiserror::Error;

/// Rejection reasons for encrypted Tribute day operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TributeDayRejectReason {
    /// Predecessor is frozen, or belongs to another day or chain.
    #[error("invalid day predecessor")]
    InvalidDayPredecessor,
    /// The Tribute record is not this day or chain.
    #[error("Tribute belongs to another day or chain")]
    TributeWrongDayOrChain,
    /// Issuance amount inside the Tribute is zero.
    #[error("Tribute issuance amount must be positive")]
    IssuanceAmountMustBePositive,
    /// Day nominal arithmetic overflowed or underflowed.
    #[error("day nominal amount overflow or underflow")]
    DayNominalAmountOverflowOrUnderflow,
    /// The fractions record is not version 1.
    #[error("invalid day fractions record")]
    InvalidDayFractionsRecord,
    /// Lysis fraction arithmetic failed.
    #[error("Lysis fraction computation failed")]
    LysisFractionComputationFailed,
    /// The supplied Tributes do not sum to the frozen day nominal.
    #[error("Lysis Tributes differ from the frozen day nominal")]
    LysisTributesDifferFromDayNominal,
    /// A NOD group-load predecessor was rejected.
    #[error("invalid Nod group load predecessor")]
    InvalidNodGroupLoadPredecessor,
    /// A NOD group-load total overflowed or underflowed.
    #[error("Nod group load overflow or underflow")]
    NodGroupLoadOverflowOrUnderflow,
}

/// Pure Tribute engine failure, including day and NOD issuance.
#[derive(Debug, Error)]
pub enum EngineError {
    /// Resident or AEAD failure.
    #[error(transparent)]
    Crypto(#[from] TeeError),
    /// A day or Lysis check rejected the input.
    #[error("offer rejected: {0}")]
    Day(#[source] TributeDayRejectReason),
    /// NOD encryption failed with a cause that is not a day rejection.
    #[error(transparent)]
    Nod(#[from] NodEncryptionError),
    /// A fixed offer check rejected the input.
    #[error("offer rejected: {reason}")]
    OfferRejected {
        /// Historical rejection sentence without the prefix.
        reason: &'static str,
    },
    /// Settlement cost overflowed. Display stays the historical sentence.
    #[error("offer rejected: settlement cost overflow")]
    SettlementOverflow(#[source] outbe_core_common::errors::RoundingError),
    /// Postcard or canonical JSON for a ciphertext did not serialize.
    ///
    /// Display stays `AEAD encryption failed`, matching the structural refusal
    /// these paths used before the source was retained.
    #[error("AEAD encryption failed")]
    Encode(#[from] postcard::Error),
    /// Decrypted day amount was not 32 bytes.
    ///
    /// Display stays `AEAD decryption failed`. The slice error stays attached.
    #[error("AEAD decryption failed")]
    AmountWidth(#[source] std::array::TryFromSliceError),
    /// Postcard of a decrypted day-fraction record did not decode.
    ///
    /// Display stays `AEAD decryption failed`. The postcard error stays attached.
    #[error("AEAD decryption failed")]
    FractionRead(#[source] postcard::Error),
    /// Canonical JSON for an inputs hash did not serialize.
    ///
    /// Display stays `AEAD encryption failed` for the same reason as [`Self::Encode`].
    #[error("AEAD encryption failed")]
    Inputs(#[from] serde_json::Error),
}

impl From<outbe_tee::resident::TeeCryptoError> for EngineError {
    fn from(error: outbe_tee::resident::TeeCryptoError) -> Self {
        Self::Crypto(TeeError::from(error))
    }
}

impl From<TributeDayRejectReason> for EngineError {
    fn from(reason: TributeDayRejectReason) -> Self {
        Self::Day(reason)
    }
}

/// Result of one pure Tribute engine step.
pub type EngineResult<T> = Result<T, EngineError>;
