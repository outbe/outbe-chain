//! Errors of the local MDBX store.

/// Errors that the store returns for a record or a write that breaks its rules.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// A stored record has the wrong length.
    #[error("corrupt {0} record in the store")]
    CorruptRecord(&'static str),
    /// A new leaf does not continue the stored leaves.
    #[error("leaf gap in domain {domain}: expected index {expected}, got {got}")]
    LeafGap {
        domain: u32,
        expected: u32,
        got: u32,
    },
    /// A signed root has no stored leaf.
    #[error("checkpoint {index} of domain {domain} has no leaf")]
    CheckpointWithoutLeaf { domain: u32, index: u32 },
    /// A different signed root is already stored at this index.
    #[error("checkpoint {index} of domain {domain} is already signed with a different root")]
    CheckpointConflict { domain: u32, index: u32 },
    /// The stored history of a domain was signed for another hook.
    #[error(
        "domain {domain}: the local history belongs to hook {stored}, but hookByDomain is {current}. To recover, stop the daemon and delete the database directory: every domain syncs and signs again"
    )]
    HookMismatch {
        domain: u32,
        stored: alloy_primitives::Address,
        current: alloy_primitives::Address,
    },
}
