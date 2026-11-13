//! Domain-neutral cryptographic errors for `tee_encrypt` and `tee_decrypt`.

use thiserror::Error;

/// Neutral error returned by [`crate::tee_encrypt`] and [`crate::tee_decrypt`].
///
/// Domain adapters in `outbe-enclave` map these variants into ledger-specific
/// error codes without coupling `outbe-tee` to application ledgers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TeeCryptoError {
    /// System random number generator failed when drawing an ephemeral secret or nonce.
    #[error("tee encryption rng failure")]
    RngFailure,

    /// HKDF-SHA256 expand step failed.
    #[error("hkdf expand failure")]
    HkdfExpand,

    /// HKDF-SHA256 output fill step failed.
    #[error("hkdf fill failure")]
    HkdfFill,

    /// ChaCha20Poly1305 key initialization failed.
    #[error("aead key initialization failure")]
    AeadKey,

    /// ChaCha20Poly1305 in-place sealing failed.
    #[error("aead sealing failure")]
    AeadSeal,

    /// Envelope header, version, marker, or minimum length is malformed.
    #[error("invalid encrypted envelope")]
    InvalidEnvelope,

    /// Ciphertext failed ChaCha20Poly1305 authentication tag verification.
    #[error("ciphertext decryption failed")]
    DecryptionFailed,

    /// Opened plaintext byte length did not match the expected fixed size.
    #[error("invalid plaintext length: expected {expected} bytes, got {actual}")]
    InvalidPlaintextLength {
        /// Expected byte length.
        expected: usize,
        /// Actual opened byte length.
        actual: usize,
    },
}

impl TeeCryptoError {
    pub(crate) const fn rng_failure(_: ring::error::Unspecified) -> Self {
        Self::RngFailure
    }

    pub(crate) const fn hkdf_expand(_: ring::error::Unspecified) -> Self {
        Self::HkdfExpand
    }

    pub(crate) const fn hkdf_fill(_: ring::error::Unspecified) -> Self {
        Self::HkdfFill
    }

    pub(crate) const fn aead_key(_: ring::error::Unspecified) -> Self {
        Self::AeadKey
    }

    pub(crate) const fn aead_seal(_: ring::error::Unspecified) -> Self {
        Self::AeadSeal
    }
}
