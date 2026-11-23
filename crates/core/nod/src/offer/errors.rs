//! Typed offer failures. Display sentences stay the historical text.

use std::fmt::{self, Write as _};

use outbe_core_common::confidential_tee::{ComputeError, TeeError};
use outbe_system_common::time::WorldwideDay;
use thiserror::Error;

/// Format `err` for a frozen wire string without dropping it as the source.
pub fn display_text(err: &impl fmt::Display) -> String {
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
    /// The emission day and currencies spell no series id.
    #[error("offer currencies spell no series id")]
    InvalidSeries,
    /// Encrypting the Nod failed.
    #[error("Nod encryption failed: {0}")]
    NodEncryptionFailed(#[source] crate::nod_encryption::NodEncryptionError),
    /// Binding hash derivation failed.
    #[error("failed to derive binding_hash: {0}")]
    DeriveBindingHashFailed(#[source] outbe_zk::Error),
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

/// Result of one pure offer-processing step.
pub type OfferResult<T> = Result<T, OfferError>;
