//! Typed errors for client-side offer encryption and owner-local decryption.

use thiserror::Error;

/// Errors returned by local encryption and owner-local opening helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum LocalCryptoError {
    /// System random number generator failed during offer encryption.
    #[error("offer encryption rng failure")]
    OfferEncryptionRngFailure,

    /// HKDF-SHA256 expand step failed.
    #[error("hkdf expand")]
    HkdfExpand,

    /// HKDF-SHA256 output fill step failed.
    #[error("hkdf fill")]
    HkdfFill,

    /// ChaCha20Poly1305 key initialization failed.
    #[error("aead key")]
    AeadKey,

    /// ChaCha20Poly1305 in-place sealing failed.
    #[error("aead seal")]
    AeadSeal,

    /// Encrypted Fidelity cohort blob failed envelope validation.
    #[error("invalid encrypted Fidelity cohort envelope")]
    InvalidFidelityCohortEnvelope,

    /// Fidelity view key failed AEAD key initialization.
    #[error("invalid Fidelity view key")]
    InvalidFidelityViewKey,

    /// Fidelity cohort ciphertext failed AEAD tag verification.
    #[error("Fidelity cohort decryption failed")]
    FidelityCohortDecryptionFailed,

    /// Encrypted Gratis balance blob failed envelope validation.
    #[error("invalid encrypted Gratis balance")]
    InvalidGratisBalance,

    /// Gratis view key failed AEAD key initialization.
    #[error("invalid Gratis view key")]
    InvalidGratisViewKey,

    /// Gratis balance ciphertext failed AEAD tag verification.
    #[error("Gratis balance decryption failed")]
    GratisBalanceDecryptionFailed,

    /// Opened Gratis plaintext did not have the 32-byte U256 length.
    #[error("invalid Gratis amount length")]
    InvalidGratisAmountLength,

    /// Encrypted Promis balance blob failed envelope validation.
    #[error("invalid encrypted Promis balance")]
    InvalidPromisBalance,

    /// Promis view key failed AEAD key initialization.
    #[error("invalid Promis view key")]
    InvalidPromisViewKey,

    /// Promis balance ciphertext failed AEAD tag verification.
    #[error("Promis balance decryption failed")]
    PromisBalanceDecryptionFailed,

    /// Opened Promis plaintext did not have the 32-byte U256 length.
    #[error("invalid Promis amount length")]
    InvalidPromisAmountLength,

    /// Encrypted NOD record failed structural encoding validation.
    #[error("invalid encrypted NOD encoding")]
    InvalidNodEncoding,

    /// Derived NOD amount key failed AEAD key initialization.
    #[error("invalid NOD amount key")]
    InvalidNodAmountKey,

    /// Encrypted NOD amount failed AEAD tag verification.
    #[error("NOD amount decryption failed")]
    NodAmountDecryptionFailed,

    /// Opened NOD plaintext did not have the 32-byte U256 length.
    #[error("invalid NOD amount length")]
    InvalidNodAmountLength,

    /// Encrypted Tribute record failed structural encoding validation.
    #[error("invalid encrypted Tribute encoding")]
    InvalidTributeEncoding,

    /// Derived Tribute amount key failed AEAD key initialization.
    #[error("invalid Tribute amount key")]
    InvalidTributeAmountKey,

    /// Encrypted Tribute amounts failed AEAD tag verification.
    #[error("Tribute amount decryption failed")]
    TributeAmountDecryptionFailed,

    /// Opened Tribute plaintext did not have the 64-byte amounts length.
    #[error("invalid Tribute amount length")]
    InvalidTributeAmountLength,
}

impl LocalCryptoError {
    /// Return the static error message for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OfferEncryptionRngFailure => "offer encryption rng failure",
            Self::HkdfExpand => "hkdf expand",
            Self::HkdfFill => "hkdf fill",
            Self::AeadKey => "aead key",
            Self::AeadSeal => "aead seal",
            Self::InvalidFidelityCohortEnvelope => "invalid encrypted Fidelity cohort envelope",
            Self::InvalidFidelityViewKey => "invalid Fidelity view key",
            Self::FidelityCohortDecryptionFailed => "Fidelity cohort decryption failed",
            Self::InvalidGratisBalance => "invalid encrypted Gratis balance",
            Self::InvalidGratisViewKey => "invalid Gratis view key",
            Self::GratisBalanceDecryptionFailed => "Gratis balance decryption failed",
            Self::InvalidGratisAmountLength => "invalid Gratis amount length",
            Self::InvalidPromisBalance => "invalid encrypted Promis balance",
            Self::InvalidPromisViewKey => "invalid Promis view key",
            Self::PromisBalanceDecryptionFailed => "Promis balance decryption failed",
            Self::InvalidPromisAmountLength => "invalid Promis amount length",
            Self::InvalidNodEncoding => "invalid encrypted NOD encoding",
            Self::InvalidNodAmountKey => "invalid NOD amount key",
            Self::NodAmountDecryptionFailed => "NOD amount decryption failed",
            Self::InvalidNodAmountLength => "invalid NOD amount length",
            Self::InvalidTributeEncoding => "invalid encrypted Tribute encoding",
            Self::InvalidTributeAmountKey => "invalid Tribute amount key",
            Self::TributeAmountDecryptionFailed => "Tribute amount decryption failed",
            Self::InvalidTributeAmountLength => "invalid Tribute amount length",
        }
    }

    /// Map a ring error to [`Self::OfferEncryptionRngFailure`].
    pub const fn offer_encryption_rng_failure(_: ring::error::Unspecified) -> Self {
        Self::OfferEncryptionRngFailure
    }

    /// Map a ring error to [`Self::HkdfExpand`].
    pub const fn hkdf_expand(_: ring::error::Unspecified) -> Self {
        Self::HkdfExpand
    }

    /// Map a ring error to [`Self::HkdfFill`].
    pub const fn hkdf_fill(_: ring::error::Unspecified) -> Self {
        Self::HkdfFill
    }

    /// Map a ring error to [`Self::AeadKey`].
    pub const fn aead_key(_: ring::error::Unspecified) -> Self {
        Self::AeadKey
    }

    /// Map a ring error to [`Self::AeadSeal`].
    pub const fn aead_seal(_: ring::error::Unspecified) -> Self {
        Self::AeadSeal
    }

    /// Map a slice conversion error to [`Self::InvalidGratisAmountLength`].
    pub const fn invalid_gratis_amount_length(_: std::array::TryFromSliceError) -> Self {
        Self::InvalidGratisAmountLength
    }

    /// Map a slice conversion error to [`Self::InvalidPromisAmountLength`].
    pub const fn invalid_promis_amount_length(_: std::array::TryFromSliceError) -> Self {
        Self::InvalidPromisAmountLength
    }

    /// Map a slice conversion error to [`Self::InvalidNodAmountLength`].
    pub const fn invalid_nod_amount_length(_: std::array::TryFromSliceError) -> Self {
        Self::InvalidNodAmountLength
    }

    /// Map a slice conversion error to [`Self::InvalidTributeAmountLength`].
    pub const fn invalid_tribute_amount_length(_: std::array::TryFromSliceError) -> Self {
        Self::InvalidTributeAmountLength
    }
}

impl std::ops::Deref for LocalCryptoError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for LocalCryptoError {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for LocalCryptoError {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for LocalCryptoError {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<String> for LocalCryptoError {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl From<LocalCryptoError> for &'static str {
    fn from(error: LocalCryptoError) -> Self {
        error.as_str()
    }
}

impl From<LocalCryptoError> for String {
    fn from(error: LocalCryptoError) -> Self {
        error.as_str().to_owned()
    }
}
