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
pub(crate) enum OcompCarrierTemporaryError {
    #[error("OCOMP result vote is not open until height {open_height}")]
    NotYetOpen { open_height: u64 },
    #[error("OCOMP result vote window at height {deadline_height} is due and still open")]
    DeadlineDueUnclosed { deadline_height: u64 },
    #[error("OCOMP result vote state is unavailable: {source}")]
    StateUnavailable { source: PrecompileError },
    #[error("OCOMP committed state is corrupt: {source}")]
    CorruptCommittedState { source: PrecompileError },
}

#[derive(Debug, Error)]
pub(crate) enum OcompNextBlockError {
    #[error("OCOMP carrier next block height overflow")]
    Overflow,
    #[error("OCOMP system carrier is not active at next block {0}")]
    NotActive(u64),
}

#[derive(Debug, Error)]
pub(crate) enum OcompCarrierRejectError {
    #[error("OCOMP carrier signer is not authorized for this action")]
    UnauthorizedSigner,
    #[error("OCOMP carrier authorization failed: {0}")]
    AuthorizationFailed(PrecompileError),
}

#[derive(Debug, Error)]
#[error("OCOMP system carrier rejected: {0}")]
pub(crate) struct OutbeOcompSystemCarrierPoolError(pub(crate) String);

impl OutbeOcompSystemCarrierPoolError {
    pub(crate) fn from_display(error: impl std::fmt::Display) -> Self {
        Self(error.to_string())
    }
}

impl PoolTransactionError for OutbeOcompSystemCarrierPoolError {
    fn is_bad_transaction(&self) -> bool {
        true
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
