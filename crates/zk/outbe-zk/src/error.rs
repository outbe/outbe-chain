//! Single error type for the generic protocol core.

use thiserror::Error;

/// Errors produced by cryptographic primitives and protocol logic.
#[derive(Clone, Debug, Error)]
pub enum Error {
    /// A hash/KDF operation failed.
    #[error("hash error: {0}")]
    Hash(#[source] outbe_poseidon::PoseidonError),

    /// Node-local native dispatcher or transport failure, not an invalid proof.
    #[error("crypto backend unavailable: {0}")]
    CryptoBackend(#[from] outbe_barretenberg::RuntimeError),

    /// A curve point that must be affine/non-identity was the identity.
    #[error("unexpected identity point: {0}")]
    Identity(&'static str),

    /// A signature failed to verify, or signing failed.
    #[error("signature error: {0}")]
    Signature(&'static str),

    /// Owner commitment did not match the value carried by the entity.
    #[error("owner mismatch: commit(pk, nonce) != entity.owner()")]
    OwnerMismatch,

    /// A value is outside the field modulus or the target type's range;
    /// converting it would silently reduce or truncate it.
    #[error("non-canonical field encoding: {0}")]
    NonCanonical(&'static str),

    /// An entity vector field was not a canonical set: its elements must be
    /// strictly ascending by field value (sorted, no duplicates) so the
    /// length-prefixed set encoding is deterministic and unambiguous. The
    /// caller must pre-sort and de-duplicate the vector.
    #[error("non-canonical set encoding: {0}")]
    UnsortedSet(String),

    /// A ZK proof backend (generation or verification) failed.
    #[error("zk proof error: {0}")]
    Proof(String),

    /// A Merkle / inclusion-tree operation failed (bad depth, index overflow,
    /// malformed path).
    #[error("merkle error: {0}")]
    Merkle(String),
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
