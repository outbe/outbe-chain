//! Errors of the origin chain sync and the domain discovery.

/// Errors that the sync returns for origin chain data that it cannot use.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SyncError {
    /// A new leaf does not continue the stored leaves.
    #[error("leaf gap in domain {domain}: expected index {expected}, got {got}")]
    LeafGap {
        domain: u32,
        expected: u32,
        got: u32,
    },
    /// The contracts of a domain do not match each other.
    #[error("domain {domain}: {what}")]
    OriginMismatch { domain: u32, what: String },
    /// The chain did not return the finalized block.
    #[error("the chain did not return a finalized block")]
    NoFinalizedBlock,
    /// A replayed log has another message id than the stored leaf.
    #[error("domain {domain} index {index}: the log has message id {read}, the stored leaf has {stored}")]
    ReplayMismatch {
        domain: u32,
        index: u32,
        stored: alloy_primitives::B256,
        read: alloy_primitives::B256,
    },
    /// A log has no block number (a pending log).
    #[error("InsertedIntoTree log without a block number")]
    LogWithoutBlock,
}
