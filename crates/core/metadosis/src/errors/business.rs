use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MetadosisBusinessError {
    #[error("Metadosis business failure: Lysis owner apply rejected: {error}")]
    LysisOwnerApplyRejected { error: PrecompileError },

    #[error("Metadosis business failure: day allocation overflow")]
    DayAllocationOverflow,

    #[error("Metadosis business failure: day allocation exceeds the nominal its tributes retired")]
    DayAllocationExceedsTributeNominal,

    #[error("Metadosis business failure: Lysis owner receipt mismatch")]
    LysisOwnerReceiptMismatch,

    #[error("Metadosis business failure: Lysis terminal permit mismatch")]
    LysisTerminalPermitMismatch,

    #[error(
        "Metadosis business failure: Lysis allocation and unused limit do not add up to the request limit"
    )]
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
        PrecompileError::Revert(value.to_string())
    }
}
