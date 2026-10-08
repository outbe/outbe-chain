//! Preserve typed transaction validation failures without relabeling them.

use reth_transaction_pool::error::PoolTransactionError;
use std::any::Any;

/// Pool record of the original invalid-transaction failure.
///
/// The builder does not relabel that failure as an unsupported transaction type.
#[derive(Debug)]
pub(super) struct PreservedInvalidUserTx {
    pub(super) reason: Box<dyn reth_evm::InvalidTxError>,
}

impl std::fmt::Display for PreservedInvalidUserTx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.reason, formatter)
    }
}

impl std::error::Error for PreservedInvalidUserTx {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.reason.as_ref())
    }
}

impl PoolTransactionError for PreservedInvalidUserTx {
    fn is_bad_transaction(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
