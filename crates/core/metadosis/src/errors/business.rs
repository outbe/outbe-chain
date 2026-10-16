use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisBusinessError {
    #[error("Lysis owner apply rejected: {error}")]
    LysisOwnerApplyRejected { error: PrecompileError },

    #[error("day allocation overflow")]
    DayAllocationOverflow,

    #[error("day allocation exceeds the nominal its tributes retired")]
    DayAllocationExceedsTributeNominal,

    #[error("Lysis owner receipt mismatch")]
    LysisOwnerReceiptMismatch,

    #[error("Lysis terminal permit mismatch")]
    LysisTerminalPermitMismatch,

    #[error("Lysis allocation and unused limit do not add up to the request limit")]
    LysisAllocationAndUnusedLimitMismatch,
}

impl MetadosisBusinessError {
    pub(crate) fn map_owner_apply(error: PrecompileError) -> PrecompileError {
        match error {
            PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
                Self::LysisOwnerApplyRejected { error }.into()
            }
            other => other,
        }
    }
}

impl From<MetadosisBusinessError> for PrecompileError {
    fn from(value: MetadosisBusinessError) -> Self {
        PrecompileError::Revert(format!("Metadosis business failure: {value}"))
    }
}
