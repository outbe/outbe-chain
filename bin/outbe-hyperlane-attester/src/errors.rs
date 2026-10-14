//! Typed errors for `outbe-hyperlane-attester`.

/// Errors that the Hyperlane attester daemon returns.
#[derive(Debug, thiserror::Error)]
pub enum AttesterError {
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
}
