//! Single error type for the generic protocol core.

use thiserror::Error;

/// Errors that cryptographic primitives and protocol logic return.
#[derive(Clone, Debug, Error)]
pub enum Error {
    /// A hash or KDF operation failed.
    #[error("hash error: {0}")]
    Hash(#[source] outbe_poseidon::PoseidonError),

    /// Node-local native dispatcher or transport failure, not an invalid proof.
    #[error("crypto backend unavailable: {0}")]
    CryptoBackend(#[from] outbe_barretenberg::RuntimeError),

    /// A curve point was the identity when the protocol requires a non-identity point.
    #[error("unexpected identity point: {0}")]
    Identity(&'static str),

    /// A signature failed verification or signing failed.
    #[error("signature error: {0}")]
    Signature(&'static str),

    /// The owner commitment did not match the entity owner value.
    #[error("owner mismatch: commit(pk, nonce) != entity.owner()")]
    OwnerMismatch,

    /// A value is outside the field modulus or target range.
    /// Conversion would silently reduce or truncate the value.
    #[error("non-canonical field encoding: {0}")]
    NonCanonical(&'static str),

    /// An entity vector is not a canonical set.
    /// Elements must be strictly ascending with no duplicates.
    /// The caller must sort and deduplicate the vector before encoding.
    #[error("non-canonical set encoding: {0}")]
    UnsortedSet(String),

    /// A zero-knowledge proof backend operation failed.
    #[error("zk proof error: {0}")]
    Proof(String),

    /// A Merkle tree operation failed because of an invalid depth, index, or path.
    #[error("merkle error: {0}")]
    Merkle(String),
}

impl From<outbe_poseidon::Error> for Error {
    fn from(error: outbe_poseidon::Error) -> Self {
        Self::Hash(error)
    }
}

impl Error {
    pub fn is_infrastructure(&self) -> bool {
        match self {
            Self::Hash(error) => error.is_infrastructure(),
            Self::CryptoBackend(error) => error.is_infrastructure(),
            _ => false,
        }
    }
}
