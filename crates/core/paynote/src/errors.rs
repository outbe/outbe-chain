//! Paynote domain errors.
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
pub enum PaynoteError {
    #[error("Paynote is not initialized")]
    NotInitialized,
    #[error("Paynote deposit amount must be non-zero")]
    DepositAmountZero,
    #[error("Paynote {0} is not a canonical BN254 field")]
    NonCanonicalField(&'static str),
    #[error("Paynote {0} must be non-zero")]
    MustBeNonZero(&'static str),
    #[error("Paynote proof is malformed: {0}")]
    MalformedProof(String),
    #[error("Paynote chain ID does not match runtime")]
    ChainIdMismatch,
    #[error("Paynote spend amount must be non-zero")]
    SpendAmountZero,
    #[error("Paynote root is not recent")]
    RootNotRecent,
    #[error("Paynote nullifier has already been spent")]
    NullifierSpent,
    #[error("Paynote proof is invalid")]
    ProofInvalid,
    #[error("Paynote commitment tree is full")]
    TreeFull,
    #[error("Paynote commitment already exists")]
    CommitmentExists,
    /// Fatal: the Poseidon2 sponge is total over field elements, so a failure
    /// here means the hasher itself is misconfigured, not bad user input.
    #[error("Paynote Poseidon2 hashing failed")]
    Hash,
    /// Fatal: a persisted frontier slot must always hold a canonical field
    /// word; anything else is storage corruption.
    #[error("Paynote filled-subtree slot is not a canonical field")]
    CorruptFrontier,
    /// Fatal: Barretenberg CRS initialization failure, never a user proof
    /// verdict.
    #[error("ZK verifier unavailable: {0}")]
    VerifierUnavailable(String),
}

impl From<PaynoteError> for PrecompileError {
    fn from(error: PaynoteError) -> Self {
        match error {
            PaynoteError::Hash
            | PaynoteError::CorruptFrontier
            | PaynoteError::VerifierUnavailable(_) => PrecompileError::Fatal(error.to_string()),
            _ => PrecompileError::Revert(error.to_string()),
        }
    }
}
