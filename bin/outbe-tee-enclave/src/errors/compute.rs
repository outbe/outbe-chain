//! Typed error definitions for enclave payload parsing, amount math, and ZK claims.

use std::fmt;

use outbe_tee::protocol::WorldwideDay;

/// Error from canonical amount parsing, nominal math, and token ID derivation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComputeError {
    /// The `tribute_draft_id` hex string failed hex decoding.
    #[error("invalid tribute_draft_id hex: {draft_id}")]
    InvalidDraftIdHex { draft_id: String },
    /// The decoded `tribute_draft_id` length is not 32 bytes.
    #[error("tribute_draft_id must be 32 bytes, got {len}")]
    DraftIdLength { len: usize },
    /// Poseidon token ID derivation failed.
    #[error("{0}")]
    PoseidonDigest(String),
    /// The issuance WorldwideDay VWAP is zero.
    #[error("issuance WorldwideDay VWAP is zero")]
    IssuanceVwapZero,
    /// The reference WorldwideDay VWAP is zero.
    #[error("reference WorldwideDay VWAP is zero")]
    ReferenceVwapZero,
    /// The nominal amount calculation overflowed.
    #[error("nominal amount overflow")]
    NominalAmountOverflow,
    /// The nominal denominator multiplication overflowed.
    #[error("nominal denominator overflow")]
    NominalDenominatorOverflow,
    /// A positive amount rounded down to zero nominal units.
    #[error("nominal amount rounds to zero")]
    NominalRoundsToZero,
    /// A numeric string is not a canonical u64 representation.
    #[error("{field} must be a canonical u64")]
    NonCanonicalU64 { field: &'static str },
    /// The `amount_micro` field is greater than or equal to 1,000,000.
    #[error("amount_micro must be less than 1000000")]
    AmountMicroRange,
    /// Combining base and micro amounts overflowed.
    #[error("amount overflow")]
    AmountOverflow,
}

impl From<outbe_compressed_entities::CommitmentError> for ComputeError {
    fn from(err: outbe_compressed_entities::CommitmentError) -> Self {
        Self::PoseidonDigest(err.to_string())
    }
}

/// Error from decrypted Tribute JSON payload parsing and validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// Parsing the decrypted JSON payload failed.
    #[error("failed to parse decrypted payload: {0}")]
    ParseDecryptedPayload(String),
    /// Deserializing the V2 encrypted Tribute payload failed.
    #[error("invalid encrypted Tribute payload: {0}")]
    InvalidEncryptedTributePayload(String),
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

impl PayloadError {
    /// Wrap a JSON deserialization error.
    pub fn parse_decrypted_payload(err: impl fmt::Display) -> Self {
        Self::ParseDecryptedPayload(err.to_string())
    }

    /// Wrap an encrypted Tribute payload JSON error.
    pub fn parse_encrypted_tribute_payload(err: impl fmt::Display) -> Self {
        Self::InvalidEncryptedTributePayload(err.to_string())
    }
}

/// Error from Tribute offer processing and ZK claim derivation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessError {
    /// Offer decryption failed.
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),
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
    VwapUnavailable { worldwide_day: WorldwideDay },
    /// Encrypting the output V2 Tribute record failed.
    #[error("Tribute encryption failed: {0}")]
    TributeEncryptionFailed(String),
    /// A ZK claim hex field is invalid.
    #[error("{what} must be 0x-prefixed 32-byte hex")]
    InvalidB256Hex { what: &'static str },
    /// Canonical sorting of SU IDs failed.
    #[error("invalid canonical su_ids: {0}")]
    InvalidCanonicalSuIds(String),
    /// Entity hash derivation for TributeDraftClaim failed.
    #[error("invalid canonical TributeDraft: {0}")]
    InvalidCanonicalTributeDraft(String),
    /// Binding hash derivation failed.
    #[error("failed to derive binding_hash: {0}")]
    DeriveBindingHashFailed(String),
}

impl ProcessError {
    /// Wrap an offer decryption error.
    pub fn decryption_failed(err: impl fmt::Display) -> Self {
        Self::DecryptionFailed(err.to_string())
    }

    /// Wrap a V2 Tribute encryption error.
    pub fn tribute_encryption_failed(err: impl fmt::Display) -> Self {
        Self::TributeEncryptionFailed(err.to_string())
    }

    /// Wrap a canonical SU ID set error.
    pub fn invalid_canonical_su_ids(err: impl fmt::Display) -> Self {
        Self::InvalidCanonicalSuIds(err.to_string())
    }

    /// Wrap a TributeDraft entity hash error.
    pub fn invalid_canonical_draft(err: impl fmt::Display) -> Self {
        Self::InvalidCanonicalTributeDraft(err.to_string())
    }

    /// Wrap a binding hash error.
    pub fn derive_binding_hash(err: impl fmt::Display) -> Self {
        Self::DeriveBindingHashFailed(err.to_string())
    }
}

/// Business or cryptographic rejection reasons for Promis balance transitions.
#[derive(Debug, thiserror::Error)]
pub enum PromisRejectReason {
    #[error("amount must be positive")]
    AmountMustBePositive,
    #[error("invalid address")]
    InvalidAddress,
    #[error("invalid modify authorization")]
    InvalidModifyAuthorization,
    #[error("promis balance overflow")]
    BalanceOverflow,
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error(transparent)]
    Tee(#[from] crate::errors::TeeError),
}

/// Business or cryptographic rejection reasons for Gratis balance transitions.
#[derive(Debug, thiserror::Error)]
pub enum GratisRejectReason {
    #[error("invalid modify authorization")]
    InvalidModifyAuthorization,
    #[error("modify nonce exhausted")]
    ModifyNonceExhausted,
    #[error("amount and account must be nonzero")]
    AmountAndAccountMustBeNonzero,
    #[error("collateral must not change Fidelity")]
    CollateralMustNotChangeFidelity,
    #[error("insufficient balance or balance overflow")]
    InsufficientBalanceOrOverflow,
    #[error(transparent)]
    Tee(#[from] crate::errors::TeeError),
}
