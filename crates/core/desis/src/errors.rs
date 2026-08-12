use alloy_primitives::Address;
use outbe_common::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DesisError {
    #[error("invalid worldwide day: {0}")]
    InvalidWorldwideDay(WorldwideDay),

    #[error("invalid stage transition")]
    InvalidStageTransition,

    #[error("stale bids generation: incoming {incoming}, last {last}")]
    StaleBidsGeneration { incoming: u32, last: u32 },

    #[error("pending clearing data missing for series {0}")]
    PendingClearingDataMissing(WorldwideDay),

    #[error("unauthorized origin: {0}")]
    UnauthorizedOrigin(Address),

    #[error("winning bid references an unpriced currency: {0}")]
    UnpricedReferenceCurrency(u16),
}

impl From<DesisError> for outbe_primitives::error::PrecompileError {
    fn from(e: DesisError) -> Self {
        outbe_primitives::error::PrecompileError::Revert(e.to_string())
    }
}
