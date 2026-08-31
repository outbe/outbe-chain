//! PayNote domain errors.
//!
//! User-facing failures are `Error(string)`-style reverts with stable texts.
//! Infrastructure failures — CRS initialization, a corrupt stored field word
//! — map to [`PrecompileError::Fatal`] and are never reported as "invalid
//! proof".
//!
//! Verification-phase backend errors are raised on caller-supplied proof
//! bytes and cannot be distinguished from rejected input at the backend seam,
//! so they revert rather than turning fatal (see [`crate::runtime::consume`]).
//! Promoting them would let any caller trigger a consensus-visible fatal error
//! with a malformed proof tail.

use outbe_primitives::error::PrecompileError;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PayNoteError {
    #[error("PayNote is not initialized")]
    NotInitialized,
    #[error("PayNote deposit amount must be non-zero")]
    DepositAmountZero,
    #[error("PayNote {0} is not a canonical BN254 field")]
    NonCanonicalField(&'static str),
    #[error("PayNote {0} must be non-zero")]
    MustBeNonZero(&'static str),
    #[error("PayNote proof is malformed: {0}")]
    MalformedProof(String),
    #[error("PayNote chain ID does not match runtime")]
    ChainIdMismatch,
    #[error("PayNote root is not recent")]
    RootNotRecent,
    #[error("PayNote nullifier has already been spent")]
    NullifierSpent,
    #[error("PayNote proof is invalid")]
    ProofInvalid,
    #[error("PayNote commitment tree is full")]
    TreeFull,
    #[error("PayNote commitment already exists")]
    CommitmentExists,
    /// Fatal: the Poseidon2 sponge is total over field elements, so a failure
    /// here means the hasher itself is misconfigured, not bad user input.
    #[error("PayNote Poseidon2 hashing failed")]
    Hash,
    /// Fatal: a persisted frontier slot must always hold a canonical field
    /// word; anything else is storage corruption.
    #[error("PayNote filled-subtree slot is not a canonical field")]
    CorruptFrontier,
    /// Fatal: Barretenberg CRS initialization failure, never a user proof
    /// verdict.
    #[error("ZK verifier unavailable: {0}")]
    VerifierUnavailable(String),
}

impl From<PayNoteError> for PrecompileError {
    fn from(error: PayNoteError) -> Self {
        match error {
            PayNoteError::Hash
            | PayNoteError::CorruptFrontier
            | PayNoteError::VerifierUnavailable(_) => PrecompileError::Fatal(error.to_string()),
            _ => PrecompileError::Revert(error.to_string()),
        }
    }
}
