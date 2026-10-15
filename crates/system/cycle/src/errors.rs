//! Typed error definitions for the ProtocolCycle system precompile.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the ProtocolCycle runtime.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CycleError {
    #[error("ProtocolCycle received invalid UTC day: active={active}, block={block}")]
    InvalidActiveUtcDay { active: u32, block: u32 },
    #[error("ProtocolCycle received invalid UTC day: active={active}, block={block}")]
    InvalidBlockUtcDay { active: u32, block: u32 },
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
            CycleError::InvalidActiveUtcDay { .. } => PrecompileError::Fatal(err.to_string()),
            CycleError::InvalidBlockUtcDay { .. }
            | CycleError::ActiveUtcDayAfterBlock { .. }
            | CycleError::NativeValidatorBudgetOverflow
            | CycleError::SettlementBeforeParticipationWindowsClose
            | CycleError::DailySettledWithoutMetadosisReceipt
            | CycleError::MetadosisReceiptWithoutDailySettled
            | CycleError::MetadosisTerminalOverflow
            | CycleError::ValidatorRewardGemPlanExceedsAllocation
            | CycleError::AllocationWithoutMetadosisReceipt
            | CycleError::AllocationDisagreesWithMetadosisReceipt
            | CycleError::ActiveUtcDayNotInitialized => PrecompileError::Revert(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_invalid_active_utc_day_is_fatal_and_all_other_cycle_errors_revert() {
        let fatal = CycleError::InvalidActiveUtcDay {
            active: 20_260_231,
            block: 20_260_301,
        };
        let fatal_msg = fatal.to_string();
        assert!(matches!(
            PrecompileError::from(fatal),
            PrecompileError::Fatal(msg) if msg == fatal_msg
        ));

        for err in [
            CycleError::InvalidBlockUtcDay {
                active: 20_260_228,
                block: 20_260_231,
            },
            CycleError::ActiveUtcDayAfterBlock {
                active: 20_260_820,
                block: 20_260_819,
            },
            CycleError::NativeValidatorBudgetOverflow,
            CycleError::SettlementBeforeParticipationWindowsClose,
            CycleError::DailySettledWithoutMetadosisReceipt,
            CycleError::MetadosisReceiptWithoutDailySettled,
            CycleError::MetadosisTerminalOverflow,
            CycleError::ValidatorRewardGemPlanExceedsAllocation,
            CycleError::AllocationWithoutMetadosisReceipt,
            CycleError::AllocationDisagreesWithMetadosisReceipt,
            CycleError::ActiveUtcDayNotInitialized,
        ] {
            let msg = err.to_string();
            assert!(matches!(
                PrecompileError::from(err),
                PrecompileError::Revert(m) if m == msg
            ));
        }
    }
}
