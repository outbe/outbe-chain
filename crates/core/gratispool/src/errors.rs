//! Error taxonomy for the shielded gratis pool.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GratisPoolError {
    #[error("denomination id out of range")]
    DenomUnknown,
    #[error("commitment already exists in the pool")]
    CommitmentDuplicate,
    #[error("merkle root is not in the recent-roots window")]
    RootStale,
    #[error("nullifier has already been spent")]
    NullifierSpent,
    #[error("field-element input is not in canonical form (>= scalar field modulus)")]
    NonCanonicalFieldInput,
    #[error("zk proof verification failed")]
    ProofInvalid,
    #[error("receiver binding does not match the proof's public input")]
    ReceiverBindingMismatch,
    #[error("only credisfactory may insert reclaim commitments")]
    UnauthorisedReclaimInsert,
    #[error("merkle tree is full for this denomination")]
    TreeFull,
    #[error("poseidon hash failure: {0}")]
    PoseidonFailed(String),
}

impl From<GratisPoolError> for PrecompileError {
    fn from(err: GratisPoolError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
