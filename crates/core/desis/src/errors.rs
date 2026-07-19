use alloy_primitives::Address;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DesisError {
    #[error("invalid worldwide day: {0}")]
    InvalidWorldwideDay(u32),

    #[error("invalid stage transition")]
    InvalidStageTransition,

    #[error("stale bids generation: incoming {incoming}, last {last}")]
    StaleBidsGeneration { incoming: u32, last: u32 },

    #[error("pending clearing data missing for series {0}")]
    PendingClearingDataMissing(u32),

    #[error("unauthorized origin: {0}")]
    UnauthorizedOrigin(Address),
}

impl From<DesisError> for outbe_primitives::error::PrecompileError {
    fn from(e: DesisError) -> Self {
        outbe_primitives::error::PrecompileError::Revert(e.to_string())
    }
}
