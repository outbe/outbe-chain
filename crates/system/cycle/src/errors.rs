//! Typed error definitions for the ProtocolCycle system precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the ProtocolCycle runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CycleError {
    #[error("ProtocolCycle received invalid UTC day: active={active}, block={block}")]
    InvalidUtcDay { active: u32, block: u32 },
    #[error("ProtocolCycle active UTC day {active} is after block UTC day {block}")]
    ActiveUtcDayAfterBlock { active: u32, block: u32 },
    #[error("native validator budget overflow")]
    NativeValidatorBudgetOverflow,
    #[error("Cycle settlement before reward participation windows close")]
    SettlementBeforeParticipationWindowsClose,
    #[error("Cycle daily_settled marker has no Metadosis day-limit semantic receipt")]
    DailySettledWithoutMetadosisReceipt,
    #[error("Metadosis day-limit semantic receipt has no Cycle daily_settled marker")]
    MetadosisReceiptWithoutDailySettled,
    #[error("metadosis terminal overflow")]
    MetadosisTerminalOverflow,
    #[error("validator reward Gem plan exceeds allocation")]
    ValidatorRewardGemPlanExceedsAllocation,
    #[error("Cycle allocation committed without a Metadosis day-limit receipt")]
    AllocationWithoutMetadosisReceipt,
    #[error("Cycle allocation disagrees with the Metadosis day-limit receipt")]
    AllocationDisagreesWithMetadosisReceipt,
    #[error("ProtocolCycle active UTC day is not initialized in genesis")]
    ActiveUtcDayNotInitialized,
}

impl From<CycleError> for PrecompileError {
    fn from(err: CycleError) -> Self {
        match err {
            CycleError::NativeValidatorBudgetOverflow
            | CycleError::MetadosisTerminalOverflow
            | CycleError::ValidatorRewardGemPlanExceedsAllocation => {
                PrecompileError::Revert(err.to_string())
            }
            _ => PrecompileError::Fatal(err.to_string()),
        }
    }
}
