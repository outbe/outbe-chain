//! Error types for the pending vote journal.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that occur when the feeder opens or updates the pending vote journal.
#[derive(Debug, Error)]
pub(crate) enum JournalError {
    /// The journal identity string is empty.
    #[error("pending vote journal identity must not be empty")]
    EmptyIdentity,
    /// The journal file has an unsupported version number.
    #[error("unsupported pending vote journal version {0}")]
    UnsupportedVersion(u32),
    /// The journal file belongs to another chain or signer.
    #[error("pending vote journal belongs to a different chain or signer")]
    IdentityMismatch,
    /// A caller tried to overwrite an unresolved pending vote.
    #[error("cannot replace an unresolved pending vote")]
    UnresolvedPendingVote,
    /// A caller tried to replace a vote when no pending vote exists.
    #[error("cannot replace a vote without an unresolved pending transaction")]
    NoPendingTransaction,
    /// The replacement transaction uses a different nonce.
    #[error("replacement must retain the unresolved nonce")]
    NonceMismatch,
    /// The replacement transaction has the same hash or raw bytes.
    #[error("replacement must be a different signed transaction")]
    IdenticalTransaction,
    /// The replacement transaction does not increase the fee cap.
    #[error("replacement fee cap must increase")]
    FeeCapNotIncreased,
    /// The replacement transaction changes the original creation timestamp.
    #[error("replacement must preserve the original pending age")]
    PendingAgeChanged,
    /// A previous write failed after rename, so disk state is uncertain.
    #[error("journal durability is uncertain; restart and reconcile before submitting")]
    DurabilityUncertain,
    /// The journal path or lock path is a symbolic link.
    #[error("journal path must not be a symlink: {}", .0.display())]
    SymlinkPath(PathBuf),
}
