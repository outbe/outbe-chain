use outbe_common::WorldwideDay;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisError {
    #[error("worldwide day type is UNKNOWN")]
    UnknownWorldwideDayType,

    #[error("VWAP must be non-zero")]
    VwapMustBeNonZero,

    #[error("cannot mark WWD {wwd} as COMPLETED from status {current} (requires READY)")]
    InvalidTransitionToCompleted { wwd: WorldwideDay, current: u8 },

    #[error("cannot mark WWD {wwd} as FAILED: day is already COMPLETED")]
    InvalidTransitionToFailed { wwd: WorldwideDay },
}

impl From<MetadosisError> for PrecompileError {
    fn from(value: MetadosisError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

pub type MetadosisResult<T> = std::result::Result<T, MetadosisError>;
