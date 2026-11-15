//! Error types for the transaction pool.

use outbe_primitives::{error::PrecompileError, zero_fee_types::ZeroFeePolicyError};
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

/// Typed policy and provider failures retained by pool admission.
#[derive(Debug, Error)]
pub enum OutbeZeroFeePoolError {
    /// A protocol policy rejected the transaction.
    #[error("zero-fee policy rejected transaction: {0}")]
    Policy(#[source] ZeroFeePolicyError),
    /// The latest committed state could not be read.
    #[error("zero-fee policy rejected transaction: {0}")]
    StateRead(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl OutbeZeroFeePoolError {
    pub(crate) fn state_read(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::StateRead(Box::new(error))
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
