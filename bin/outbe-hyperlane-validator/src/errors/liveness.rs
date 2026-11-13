//! Errors of the transactions that the liveness, the announce and the
//! daemon send.

/// Errors that the validator returns when a transaction fails.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LivenessError {
    /// The RPC provider rejected the transaction.
    #[error("transaction failed: {0}")]
    TransactionFailed(String),
    /// The transaction is in a block, but it reverted.
    #[error("transaction {0} reverted")]
    Reverted(alloy_primitives::B256),
    /// The transaction has no receipt before the time limit.
    #[error("transaction {0} has no receipt after the time limit")]
    NoReceipt(alloy_primitives::B256),
}
