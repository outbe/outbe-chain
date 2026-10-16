use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisCallerError {
    #[error("formed day limit cannot be replaced")]
    FormedDayLimitCannotBeReplaced,

    #[error("unknown WorldwideDay status")]
    UnknownWorldwideDayStatus,

    #[error("formed day limit is immutable")]
    FormedDayLimitImmutable,

    #[error("Metadosis forming window overflow")]
    FormingWindowOverflow,

    #[error("Metadosis lookback window overflow")]
    LookbackWindowOverflow,

    #[error("Metadosis offering window overflow")]
    OfferingWindowOverflow,

    #[error("Metadosis waiting window overflow")]
    WaitingWindowOverflow,

    #[error("Metadosis {label} duration overflow")]
    DurationOverflow { label: &'static str },

    #[error("Metadosis UTC+14 timestamp overflow")]
    UtcPlus14TimestampOverflow,
}

impl From<MetadosisCallerError> for PrecompileError {
    fn from(value: MetadosisCallerError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
