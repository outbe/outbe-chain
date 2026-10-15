//! V2 Phase 1 accounting errors.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AccountingError {
    #[error("last_accounted_block_number regression: current={current}, attempted={attempted}")]
    BlockNumberRegression { current: u64, attempted: u64 },
}

impl From<AccountingError> for PrecompileError {
    fn from(err: AccountingError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
