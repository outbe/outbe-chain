use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisStorageCorruptionError {
    #[error("Metadosis EmissionDay {day} WWD link mismatch (expected {expected_wwd}, found {found_wwd})")]
    EmissionDayWwdLinkMismatch {
        day: u32,
        expected_wwd: WorldwideDay,
        found_wwd: WorldwideDay,
    },
}

impl From<MetadosisStorageCorruptionError> for PrecompileError {
    fn from(value: MetadosisStorageCorruptionError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
