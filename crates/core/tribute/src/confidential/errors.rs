//! Typed Tribute engine failures. Display sentences stay the historical text.
//!
//! A wire `Rejected { reason }` stores [`display_text`] of the typed error.
//! The source chain is not erased.

use std::fmt::{self, Write as _};

use outbe_core_common::confidential_tee::{ComputeError, TeeError};
use outbe_nod::nod_encryption::NodEncryptionError;
use outbe_system_common::time::WorldwideDay;
use thiserror::Error;

/// Format `err` for a frozen wire string without dropping it as the source.
pub(crate) fn display_text(err: &impl fmt::Display) -> String {
    let mut reason = String::new();
    let _ = write!(&mut reason, "{err}");
    reason
}

/// Error from decrypted Tribute JSON payload parsing and validation.
#[derive(Debug, Error)]
pub enum PayloadError {
    /// Parsing the decrypted JSON payload failed.
    #[error("failed to parse decrypted payload: {0}")]
    ParseDecryptedPayload(#[source] serde_json::Error),
    /// Deserializing the V2 encrypted Tribute payload failed.
    #[error("invalid encrypted Tribute payload: {0}")]
    InvalidEncryptedTributePayload(#[source] serde_json::Error),
    /// The `tribute_draft_id` field is empty.
    #[error("tribute_draft_id is required")]
    DraftIdRequired,
    /// The `creator` field is empty.
    #[error("creator is required")]
    CreatorRequired,
    /// The `su_hashes` list is empty.
    #[error("su_hashes cannot be empty")]
    SuHashesEmpty,
}

/// Error from Tribute offer processing and ZK claim derivation.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// Offer decryption failed.
    #[error("decryption failed: {0}")]
    DecryptionFailed(#[source] TeeError),
    /// The offer ZK proof chain ID differs from the resident chain ID.
    #[error("offer proof chain differs from resident chain")]
    OfferProofChainMismatch,
    /// The `creator` string is not a valid L1 address.
    #[error("creator must be an L1 address")]
    InvalidCreatorAddress,
    /// The parsed amount is zero.
    #[error("amount must be positive")]
    AmountNotPositive,
    /// A required WorldwideDay VWAP price is zero.
    #[error("required WorldwideDay VWAP unavailable for worldwide_day {worldwide_day}")]
    VwapUnavailable {
        /// Day whose VWAP was missing.
        worldwide_day: WorldwideDay,
    },
    /// Encrypting the output V2 Tribute record failed.
    #[error("Tribute encryption failed: {0}")]
    TributeEncryptionFailed(#[source] TeeError),
    /// A ZK claim hex field is invalid.
    #[error("{what} must be 0x-prefixed 32-byte hex")]
    InvalidB256Hex {
        /// Field name that failed.
        what: &'static str,
    },
    /// Canonical sorting of SU IDs failed.
    #[error("invalid canonical su_ids: {0}")]
    InvalidCanonicalSuIds(#[source] outbe_zk::Error),
    /// Entity hash derivation for TributeDraftClaim failed.
    #[error("invalid canonical TributeDraft: {0}")]
    InvalidCanonicalTributeDraft(#[source] outbe_zk::Error),
    /// Binding hash derivation failed.
    #[error("failed to derive binding_hash: {0}")]
    DeriveBindingHashFailed(#[source] outbe_zk::Error),
}

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

/// One offer failed before a public result could be built.
#[derive(Debug, Error)]
pub enum OfferError {
    /// Canonical amount or token-id math failed.
    #[error(transparent)]
    Compute(#[from] ComputeError),
    /// The decrypted JSON payload was rejected.
    #[error(transparent)]
    Payload(#[from] PayloadError),
    /// Offer processing or the ZK claim failed.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// Resident cryptography failed.
    #[error(transparent)]
    Crypto(#[from] TeeError),
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

/// Result of one pure offer-processing step.
pub type OfferResult<T> = Result<T, OfferError>;

/// Result of one pure Tribute engine step.
pub type EngineResult<T> = Result<T, EngineError>;
