//! PayNote domain errors.
//!
//! User-facing failures are `Error(string)`-style reverts with stable texts.
//! Infrastructure failures, such as CRS initialization or a corrupt stored
//! field word, map to [`PrecompileError::Fatal`]. They never appear as "invalid
//! proof".
//!
//! Verification-phase backend errors occur on caller-supplied proof bytes. At
//! the backend seam, they cannot be distinguished from rejected input. Thus
//! they revert rather than turning fatal (see [`crate::runtime::consume`]).
//! Promoting them would let any caller trigger a consensus-visible fatal error
//! with a malformed proof tail.

use outbe_primitives::error::PrecompileError;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PayNoteError {
    #[error("PayNote is not initialized")]
    NotInitialized,
    /// A guard rejected caller-supplied input. The payload is the reason,
    /// rendered as `PayNote <reason>`.
    #[error("PayNote {0}")]
    InvalidInput(String),
    #[error("PayNote root is not recent")]
    RootNotRecent,
    #[error("PayNote nullifier has already been spent")]
    NullifierSpent,
    #[error("PayNote commitment tree is full")]
    TreeFull,
    #[error("PayNote commitment already exists")]
    CommitmentExists,
    #[error("PayNote token call failed")]
    TokenOperationFailed,
    #[error("PayNote token moved an unexpected amount")]
    DepositAmountMismatch,
    /// Fatal: the Poseidon2 sponge is total over field elements. Thus a failure
    /// here means the hasher itself is misconfigured, not bad user input.
    #[error("PayNote Poseidon2 hashing failed")]
    Hash,
    /// Fatal: a persisted frontier slot must always hold a canonical field
    /// word. Anything else is storage corruption.
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
