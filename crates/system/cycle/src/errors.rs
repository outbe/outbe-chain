//! Typed error definitions for the ProtocolCycle system precompile.

use crate::ICycle;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors returned by the ProtocolCycle runtime.
#[derive(Debug, Error, PartialEq, Eq)]
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
    #[error("validator top-up exceeds allocation")]
    ValidatorTopupExceedsAllocation,
    #[error("Cycle allocation committed without a Metadosis day-limit receipt")]
    AllocationWithoutMetadosisReceipt,
    #[error("Cycle allocation disagrees with the Metadosis day-limit receipt")]
    AllocationDisagreesWithMetadosisReceipt,
    #[error("ProtocolCycle active UTC day is not initialized in genesis")]
    ActiveUtcDayNotInitialized,
    #[error("last_accounted_block_number regression: current={current}, attempted={attempted}")]
    BlockNumberRegression { current: u64, attempted: u64 },
    #[error("accounting progress input is zero")]
    ZeroAccountingProgress,
    #[error("accounting progress is not the next block: current={current}, attempted={attempted}")]
    AccountingProgressGap { current: u64, attempted: u64 },
    #[error("trigger period is zero")]
    ZeroTriggerPeriod,
    #[error(
        "schedule slot does not fit in u64: period={period}, offset={offset}, timestamp={timestamp}"
    )]
    ScheduleArithmeticOverflow {
        period: u64,
        offset: u64,
        timestamp: u64,
    },
}

impl From<CycleError> for PrecompileError {
    fn from(error: CycleError) -> Self {
        match error {
            error @ CycleError::InvalidActiveUtcDay {
                active: _,
                block: _,
            } => PrecompileError::Fatal(error.to_string()),
            CycleError::InvalidBlockUtcDay { active, block } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICycle::InvalidBlockUtcDay {
                        active,
                        blockDay: block,
                    }
                    .abi_encode(),
                ))
            }
            CycleError::ActiveUtcDayAfterBlock { active, block } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICycle::ActiveUtcDayAfterBlock {
                        active,
                        blockDay: block,
                    }
                    .abi_encode(),
                ))
            }
            CycleError::NativeValidatorBudgetOverflow => PrecompileError::RevertBytes(Bytes::from(
                ICycle::NativeValidatorBudgetOverflow {}.abi_encode(),
            )),
            CycleError::SettlementBeforeParticipationWindowsClose => PrecompileError::RevertBytes(
                Bytes::from(ICycle::SettlementBeforeParticipationWindowsClose {}.abi_encode()),
            ),
            CycleError::DailySettledWithoutMetadosisReceipt => PrecompileError::RevertBytes(
                Bytes::from(ICycle::DailySettledWithoutMetadosisReceipt {}.abi_encode()),
            ),
            CycleError::MetadosisReceiptWithoutDailySettled => PrecompileError::RevertBytes(
                Bytes::from(ICycle::MetadosisReceiptWithoutDailySettled {}.abi_encode()),
            ),
            CycleError::MetadosisTerminalOverflow => PrecompileError::RevertBytes(Bytes::from(
                ICycle::MetadosisTerminalOverflow {}.abi_encode(),
            )),
            CycleError::ValidatorTopupExceedsAllocation => PrecompileError::RevertBytes(
                Bytes::from(ICycle::ValidatorTopupExceedsAllocation {}.abi_encode()),
            ),
            CycleError::AllocationWithoutMetadosisReceipt => PrecompileError::RevertBytes(
                Bytes::from(ICycle::AllocationWithoutMetadosisReceipt {}.abi_encode()),
            ),
            CycleError::AllocationDisagreesWithMetadosisReceipt => PrecompileError::RevertBytes(
                Bytes::from(ICycle::AllocationDisagreesWithMetadosisReceipt {}.abi_encode()),
            ),
            CycleError::ActiveUtcDayNotInitialized => PrecompileError::RevertBytes(Bytes::from(
                ICycle::ActiveUtcDayNotInitialized {}.abi_encode(),
            )),
            CycleError::BlockNumberRegression { current, attempted } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICycle::BlockNumberRegression { current, attempted }.abi_encode(),
                ))
            }
            CycleError::ZeroAccountingProgress => PrecompileError::RevertBytes(Bytes::from(
                ICycle::ZeroAccountingProgress {}.abi_encode(),
            )),
            CycleError::AccountingProgressGap { current, attempted } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ICycle::AccountingProgressGap { current, attempted }.abi_encode(),
                ))
            }
            CycleError::ZeroTriggerPeriod => {
                PrecompileError::RevertBytes(Bytes::from(ICycle::ZeroTriggerPeriod {}.abi_encode()))
            }
            CycleError::ScheduleArithmeticOverflow {
                period,
                offset,
                timestamp,
            } => PrecompileError::RevertBytes(Bytes::from(
                ICycle::ScheduleArithmeticOverflow {
                    period,
                    offset,
                    timestamp,
                }
                .abi_encode(),
            )),
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

        let cases = [
            (
                CycleError::InvalidBlockUtcDay {
                    active: 20_260_228,
                    block: 20_260_231,
                },
                ICycle::InvalidBlockUtcDay::SELECTOR,
            ),
            (
                CycleError::ActiveUtcDayAfterBlock {
                    active: 20_260_820,
                    block: 20_260_819,
                },
                ICycle::ActiveUtcDayAfterBlock::SELECTOR,
            ),
            (
                CycleError::NativeValidatorBudgetOverflow,
                ICycle::NativeValidatorBudgetOverflow::SELECTOR,
            ),
            (
                CycleError::SettlementBeforeParticipationWindowsClose,
                ICycle::SettlementBeforeParticipationWindowsClose::SELECTOR,
            ),
            (
                CycleError::DailySettledWithoutMetadosisReceipt,
                ICycle::DailySettledWithoutMetadosisReceipt::SELECTOR,
            ),
            (
                CycleError::MetadosisReceiptWithoutDailySettled,
                ICycle::MetadosisReceiptWithoutDailySettled::SELECTOR,
            ),
            (
                CycleError::MetadosisTerminalOverflow,
                ICycle::MetadosisTerminalOverflow::SELECTOR,
            ),
            (
                CycleError::ValidatorTopupExceedsAllocation,
                ICycle::ValidatorTopupExceedsAllocation::SELECTOR,
            ),
            (
                CycleError::AllocationWithoutMetadosisReceipt,
                ICycle::AllocationWithoutMetadosisReceipt::SELECTOR,
            ),
            (
                CycleError::AllocationDisagreesWithMetadosisReceipt,
                ICycle::AllocationDisagreesWithMetadosisReceipt::SELECTOR,
            ),
            (
                CycleError::ActiveUtcDayNotInitialized,
                ICycle::ActiveUtcDayNotInitialized::SELECTOR,
            ),
            (
                CycleError::BlockNumberRegression {
                    current: 10,
                    attempted: 9,
                },
                ICycle::BlockNumberRegression::SELECTOR,
            ),
            (
                CycleError::ZeroAccountingProgress,
                ICycle::ZeroAccountingProgress::SELECTOR,
            ),
            (
                CycleError::AccountingProgressGap {
                    current: 4,
                    attempted: 9,
                },
                ICycle::AccountingProgressGap::SELECTOR,
            ),
            (
                CycleError::ZeroTriggerPeriod,
                ICycle::ZeroTriggerPeriod::SELECTOR,
            ),
            (
                CycleError::ScheduleArithmeticOverflow {
                    period: 2,
                    offset: 0,
                    timestamp: u64::MAX,
                },
                ICycle::ScheduleArithmeticOverflow::SELECTOR,
            ),
        ];
        for (err, selector) in cases {
            match PrecompileError::from(err) {
                PrecompileError::RevertBytes(bytes) => assert_eq!(&bytes[..4], &selector),
                other => panic!("expected typed revert bytes, got {other:?}"),
            }
        }
        let encoded = match PrecompileError::from(CycleError::InvalidBlockUtcDay {
            active: 20_260_228,
            block: 20_260_231,
        }) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let decoded = ICycle::InvalidBlockUtcDay::abi_decode(&encoded).expect("day payload");
        assert_eq!(decoded.active, 20_260_228);
        assert_eq!(decoded.blockDay, 20_260_231);

        let overflow = match PrecompileError::from(CycleError::ScheduleArithmeticOverflow {
            period: 100,
            offset: u64::MAX - 50,
            timestamp: u64::MAX - 50,
        }) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let decoded =
            ICycle::ScheduleArithmeticOverflow::abi_decode(&overflow).expect("overflow payload");
        assert_eq!(decoded.period, 100);
        assert_eq!(decoded.offset, u64::MAX - 50);
        assert_eq!(decoded.timestamp, u64::MAX - 50);

        let gap = match PrecompileError::from(CycleError::AccountingProgressGap {
            current: 4,
            attempted: 9,
        }) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let decoded = ICycle::AccountingProgressGap::abi_decode(&gap).expect("gap payload");
        assert_eq!(decoded.current, 4);
        assert_eq!(decoded.attempted, 9);
    }
}
