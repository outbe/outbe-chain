//! Typed errors for `outbe-hyperlane-validator`.

/// Errors that the Hyperlane validator daemon returns.
#[derive(Debug, thiserror::Error)]
pub enum ValidatorError {
    /// The storage location does not start with `s3+`.
    #[error("unsupported storage location '{0}'")]
    UnsupportedStorageLocation(String),
    /// The storage location does not contain `://`.
    #[error("malformed storage location '{0}'")]
    MalformedStorageLocation(String),
    /// The storage location does not include a bucket name.
    #[error("storage location '{0}' has no bucket")]
    StorageLocationMissingBucket(String),
    /// The RPC provider rejected the transaction.
    #[error("transaction failed: {0}")]
    TransactionFailed(String),
    /// The validator address in the configuration is not a 20-byte hex address.
    #[error("account.validator_address is not a valid 20-byte hex address: {0}")]
    InvalidValidatorAddress(String),
    /// The poll interval in the configuration is zero.
    #[error("hyperlane.poll_interval_secs must be > 0")]
    ZeroPollInterval,
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
    /// A `db` command needs `[storage] db_path` in the configuration.
    #[error("storage.db_path is not set in the configuration")]
    NoDbPath,
}
