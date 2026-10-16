mod caller;
mod corruption;
#[cfg(any(test, feature = "test-utils"))]
mod test_support;
pub use caller::MetadosisCallerError;
pub use corruption::MetadosisStorageCorruptionError;
#[cfg(any(test, feature = "test-utils"))]
pub use test_support::MetadosisTestSupportError;

use alloy_primitives::U256;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisError {
    #[error("Metadosis business failure: worldwide day type is UNKNOWN")]
    UnknownWorldwideDayType,

    #[error("Metadosis business failure: VWAP must be non-zero")]
    VwapMustBeNonZero,

    #[error(
        "Metadosis business failure: invalid Lysis limit split: lysis limit {lysis_limit_minor} exceeds day limit {day_limit}"
    )]
    InvalidLysisLimitSplit {
        day_limit: U256,
        lysis_limit_minor: U256,
    },

    #[error("Lysis carry-over credit does not match the immutable day split")]
    LysisLimitCreditMismatch,

    #[error(
        "Metadosis business failure: Desis Limit {desis_limit_minor} exceeds the {available} the Promis Limit holds"
    )]
    DesisLimitUnavailable {
        desis_limit_minor: U256,
        available: U256,
    },
}

impl From<MetadosisError> for PrecompileError {
    fn from(value: MetadosisError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}

pub type MetadosisResult<T> = std::result::Result<T, MetadosisError>;

pub(crate) fn is_vwap_overflow(error: &PrecompileError) -> bool {
    matches!(error, PrecompileError::Revert(message) if message.starts_with("VWAP overflow:"))
}
