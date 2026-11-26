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
        "Metadosis business failure: invalid Metadosis limit split: Nod loads {nod_loads_minor} exceed day limit {day_limit}"
    )]
    InvalidMetadosisLimitSplit {
        day_limit: U256,
        nod_loads_minor: U256,
    },

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
            MetadosisError::InvalidMetadosisLimitSplit {
                day_limit,
                nod_loads_minor,
            } => revert_with(I::InvalidMetadosisLimitSplit {
                dayLimit: day_limit,
                nodLoadsMinor: nod_loads_minor,
            }),
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
