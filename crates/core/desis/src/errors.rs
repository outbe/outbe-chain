use thiserror::Error;

#[derive(Debug, Error)]
pub enum DesisError {
    #[error("invalid series id: {0}")]
    InvalidSeriesId(u32),

    #[error("invalid stage transition")]
    InvalidStageTransition,

    #[error("stale bids generation: incoming {incoming}, last {last}")]
    StaleBidsGeneration { incoming: u32, last: u32 },

    #[error("pending clearing data missing for series {0}")]
    PendingClearingDataMissing(u32),
}

impl From<DesisError> for outbe_primitives::error::PrecompileError {
    fn from(e: DesisError) -> Self {
        outbe_primitives::error::PrecompileError::Revert(e.to_string())
    }
}
