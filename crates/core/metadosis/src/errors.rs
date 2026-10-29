mod caller;
mod corruption;
#[cfg(any(test, feature = "test-utils"))]
mod test_support;
pub use caller::MetadosisCallerError;
pub use corruption::MetadosisStorageCorruptionError;
#[cfg(any(test, feature = "test-utils"))]
pub use test_support::MetadosisTestSupportError;

use crate::precompile::IMetadosis;
use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisError {
    #[error("Metadosis business failure: VWAP must be non-zero")]
    VwapMustBeNonZero,

    #[error(
        "Metadosis business failure: invalid Lysis limit split: lysis limit {lysis_limit_minor} exceeds day limit {day_limit}"
    )]
    InvalidLysisLimitSplit {
        day_limit: U256,
        lysis_limit_minor: U256,
    },

    #[error("Lysis Promis Limit credit does not match the immutable day split")]
    LysisLimitCreditMismatch,

    #[error(
        "Metadosis business failure: Desis Limit {desis_limit_minor} exceeds the {available} the Promis Limit holds"
    )]
    DesisLimitUnavailable {
        desis_limit_minor: U256,
        available: U256,
    },
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<MetadosisError> for PrecompileError {
    fn from(value: MetadosisError) -> Self {
        use IMetadosis as I;
        match value {
            MetadosisError::VwapMustBeNonZero => revert_with(I::VwapMustBeNonZero {}),
            MetadosisError::InvalidLysisLimitSplit {
                day_limit,
                lysis_limit_minor,
            } => revert_with(I::InvalidLysisLimitSplit {
                dayLimit: day_limit,
                lysisLimitMinor: lysis_limit_minor,
            }),
            MetadosisError::LysisLimitCreditMismatch => revert_with(I::LysisLimitCreditMismatch {}),
            MetadosisError::DesisLimitUnavailable {
                desis_limit_minor,
                available,
            } => revert_with(I::DesisLimitUnavailable {
                desisLimitMinor: desis_limit_minor,
                available,
            }),
        }
    }
}

pub type MetadosisResult<T> = std::result::Result<T, MetadosisError>;
