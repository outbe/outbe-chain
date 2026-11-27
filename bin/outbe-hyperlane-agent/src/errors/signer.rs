//! Errors of the per-domain signer.

/// Errors that the signer returns when it must not sign.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SignerError {
    /// A stored signed root differs from the root that the stored leaves
    /// give at the same index.
    #[error(
        "domain {domain} index {index}: signed root {signed} differs from the leaf root {derived}"
    )]
    RootConflict {
        domain: u32,
        index: u32,
        signed: alloy_primitives::B256,
        derived: alloy_primitives::B256,
    },
    /// The domain halted after a reorg. It signs nothing more.
    #[error("domain {0} is halted after a reorg")]
    Halted(u32),
}
