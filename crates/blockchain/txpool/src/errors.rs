//! Error types for the transaction pool.

use outbe_primitives::error::PrecompileError;
use reth_transaction_pool::error::PoolTransactionError;
use std::any::Any;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("reserved system transaction address is not accepted from users")]
pub(crate) struct OutbeReservedSystemTxPoolError;

impl PoolTransactionError for OutbeReservedSystemTxPoolError {
    fn is_bad_transaction(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Error)]
#[error("zero-fee policy rejected transaction: {0}")]
pub(crate) struct OutbeZeroFeePoolError(pub(crate) String);

impl OutbeZeroFeePoolError {
    pub(crate) fn from_display(error: impl std::fmt::Display) -> Self {
        Self(error.to_string())
    }
}

impl PoolTransactionError for OutbeZeroFeePoolError {
    fn is_bad_transaction(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Error)]
pub(crate) enum TxPoolStateReadError {
    #[error("state read failed: {0}")]
    StateReadFailed(String),
}

impl From<TxPoolStateReadError> for PrecompileError {
    fn from(error: TxPoolStateReadError) -> Self {
        Self::Storage(error.to_string())
    }
}
