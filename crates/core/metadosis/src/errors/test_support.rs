use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisTestSupportError {
    #[error("capacity fixture exceeds MAX_RETAINED_WWDS")]
    CapacityFixtureExceedsMaxRetainedWwds,

    #[error("capacity fixture contains an invalid WorldwideDay")]
    CapacityFixtureInvalidWorldwideDay,

    #[error("Metadosis active WWD cap {max_active_wwds} reached before inserting {wwd}")]
    ActiveWwdCapReached {
        max_active_wwds: usize,
        wwd: WorldwideDay,
    },
}

impl From<MetadosisTestSupportError> for PrecompileError {
    fn from(value: MetadosisTestSupportError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
