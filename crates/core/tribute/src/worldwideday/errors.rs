use crate::precompile::ITribute;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WwdCallerError {
    #[error("Metadosis forming window overflow")]
    FormingWindowOverflow,

    #[error("Metadosis lookback window overflow")]
    LookbackWindowOverflow,

    #[error("Metadosis offering window overflow")]
    OfferingWindowOverflow,

    #[error("Metadosis {label} duration overflow")]
    DurationOverflow { label: &'static str },

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<WwdCallerError> for PrecompileError {
    fn from(value: WwdCallerError) -> Self {
        use ITribute as I;
        match value {
            WwdCallerError::FormingWindowOverflow => revert_with(I::FormingWindowOverflow {}),
            WwdCallerError::LookbackWindowOverflow => revert_with(I::LookbackWindowOverflow {}),
            WwdCallerError::OfferingWindowOverflow => revert_with(I::OfferingWindowOverflow {}),
            WwdCallerError::DurationOverflow { label } => revert_with(I::DurationOverflow {
                label: label.to_string(),
            }),
            WwdCallerError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WwdStorageCorruptionError {
    #[error("Metadosis WWD {wwd} already exists with different linked day (expected {expected_day}, found {found_day})")]
    WwdDayLinkMismatch {
        wwd: WorldwideDay,
        expected_day: u32,
        found_day: u32,
    },

    #[error("Metadosis active WWD cap {max_active_wwds} reached before inserting {wwd}")]
    ActiveWwdCapReached {
        max_active_wwds: usize,
        wwd: WorldwideDay,
    },

    #[error("Metadosis active WWD population {count} exceeds derived cap {max_active_wwds}")]
    ActiveWwdPopulationExceedsDerivedCap {
        count: usize,
        max_active_wwds: usize,
    },

    #[error("Metadosis active index points to missing WWD {wwd}")]
    ActiveIndexPointsToMissingWwd { wwd: WorldwideDay },

    #[error("Metadosis WWD {wwd} has non-monotonic phase boundaries")]
    WwdNonMonotonicPhaseBoundaries { wwd: WorldwideDay },

    #[error("Metadosis {label} WWD index contains duplicate {value}")]
    WwdIndexContainsDuplicate {
        label: &'static str,
        value: WorldwideDay,
    },

    #[error("created WWD record disappeared")]
    CreatedWwdRecordDisappeared,

    #[error("fresh-devnet genesis WWD is not a valid calendar day")]
    GenesisWwdNotValidCalendarDay,

    #[error("fresh-devnet genesis WWD phase order is invalid")]
    GenesisWwdPhaseOrderInvalid,

    #[error("fresh-devnet genesis builder cannot run after the pre-launch boundary")]
    GenesisBuilderCannotRunAfterPreLaunchBoundary,

    #[error("fresh-devnet genesis WWD already exists")]
    GenesisWwdAlreadyExists,

    #[error("fresh-devnet offering WWD is missing")]
    GenesisOfferingWwdMissing,

    #[error("fresh-devnet WWD phases end after the requested offering deadline")]
    GenesisWwdPhasesEndAfterOfferingDeadline,

    #[error("fresh-devnet genesis contains an unexpected active WWD")]
    GenesisContainsUnexpectedActiveWwd,

    #[error("fresh-devnet active WWD record is missing")]
    GenesisActiveWwdRecordMissing,
}

impl From<WwdStorageCorruptionError> for PrecompileError {
    fn from(value: WwdStorageCorruptionError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
