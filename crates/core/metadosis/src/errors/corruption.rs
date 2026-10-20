use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisStorageCorruptionError {
    #[error("Metadosis bootstrap needs the genesis Worldwide Day")]
    BootstrapNeedsGenesisWorldwideDay,

    #[error("genesis Worldwide Day record disappeared")]
    GenesisWorldwideDayRecordDisappeared,

    #[error("Metadosis EmissionDay {day} WWD link mismatch (expected {expected_wwd}, found {found_wwd})")]
    EmissionDayWwdLinkMismatch {
        day: u32,
        expected_wwd: WorldwideDay,
        found_wwd: WorldwideDay,
    },

    #[error("Metadosis full-precision demand overflow")]
    FullPrecisionDemandOverflow,

    #[error("Metadosis day-limit return overflow")]
    DayLimitReturnOverflow,

    #[error("Metadosis Lysis limit exceeds day nominal")]
    LysisLimitExceedsDayNominal,

    #[error("Metadosis split exceeds day limit")]
    SplitExceedsDayLimit,

    #[error("accepted Desis brief exceeds routing limit")]
    AcceptedDesisBriefExceedsRoutingLimit,

    #[error("Desis rejection receipt does not match dispatched limit")]
    DesisRejectionReceiptMismatchDispatchedLimit,
}

impl From<MetadosisStorageCorruptionError> for PrecompileError {
    fn from(value: MetadosisStorageCorruptionError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
