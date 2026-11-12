//! Errors of the transactions that the liveness, the announce and the
//! daemon send.

/// Errors that the validator returns when a transaction fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LivenessError {
    /// The RPC provider rejected the transaction.
    #[error("transaction failed: {0}")]
    TransactionFailed(String),
}
