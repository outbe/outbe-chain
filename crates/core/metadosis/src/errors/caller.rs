use crate::precompile::IMetadosis;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
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

    #[error("date step kind {kind} from date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

fn revert_with<E: SolError>(error: E) -> PrecompileError {
    PrecompileError::RevertBytes(Bytes::from(error.abi_encode()))
}

impl From<MetadosisCallerError> for PrecompileError {
    fn from(value: MetadosisCallerError) -> Self {
        use IMetadosis as I;
        match value {
            MetadosisCallerError::FormedDayLimitCannotBeReplaced => {
                revert_with(I::FormedDayLimitCannotBeReplaced {})
            }
            MetadosisCallerError::UnknownWorldwideDayStatus => {
                revert_with(I::UnknownWorldwideDayStatus {})
            }
            MetadosisCallerError::FormedDayLimitImmutable => {
                revert_with(I::FormedDayLimitImmutable {})
            }
            MetadosisCallerError::FormingWindowOverflow => revert_with(I::FormingWindowOverflow {}),
            MetadosisCallerError::LookbackWindowOverflow => {
                revert_with(I::LookbackWindowOverflow {})
            }
            MetadosisCallerError::OfferingWindowOverflow => {
                revert_with(I::OfferingWindowOverflow {})
            }
            MetadosisCallerError::WaitingWindowOverflow => revert_with(I::WaitingWindowOverflow {}),
            MetadosisCallerError::DurationOverflow { label } => revert_with(I::DurationOverflow {
                label: label.to_string(),
            }),
            MetadosisCallerError::UtcPlus14TimestampOverflow => {
                revert_with(I::UtcPlus14TimestampOverflow {})
            }
            MetadosisCallerError::DateStep { kind, date_key } => revert_with(I::DateStep {
                kind,
                dateKey: date_key,
            }),
        }
    }
}
