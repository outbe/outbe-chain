use alloy_primitives::U256;
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

    #[error(
        "invalid OCOMP budget split: lysis budget {lysis_budget} exceeds day limit {day_limit}"
    )]
    InvalidOcompBudgetSplit { day_limit: U256, lysis_budget: U256 },

    #[error(
        "existing OCOMP request budget effect nonce {effect_nonce} is ahead of current nonce {current_nonce}"
    )]
    OcompBudgetEffectFromFuture {
        effect_nonce: u64,
        current_nonce: u64,
    },

    #[error("existing OCOMP request budget receipt does not match the immutable day split")]
    OcompBudgetReceiptMismatch,

    #[error("Desis returned a different OCOMP request brief hash")]
    OcompDesisBriefHashMismatch,

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
