//! Shared error types for `outbe-common`.

use thiserror::Error;

/// Proof-of-work errors. Factories map these variants onto their own error enums.
/// The enum stays exhaustive, so a new variant requires an explicit mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PowError {
    /// The computed hash does not have `POW_DIFFICULTY` leading zero bytes.
    #[error("insufficient proof of work")]
    InsufficientProofOfWork,
}
