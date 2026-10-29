//! Error types for the Emission system precompile (`outbe-emission`).

use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::time::TimeError;
use thiserror::Error;

use crate::precompile::IEmission;

/// Represents errors returned by the Emission allocation and ledger runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EmissionLimitError {
    /// Indicates that fixed-share multiplication overflowed.
    #[error("emission fixed-share multiplication overflow")]
    FixedShareMultiplicationOverflow,
    /// Indicates that the fixed allocation total overflowed.
    #[error("emission fixed allocation total overflow")]
    FixedAllocationTotalOverflow,
    /// Indicates that terminal allocation calculation underflowed.
    #[error("emission terminal allocation underflow")]
    TerminalAllocationUnderflow,
    /// Indicates that fixed percentage summation overflowed.
    #[error("emission fixed percentage overflow")]
    FixedPercentageOverflow,
    /// Indicates that the sink table defines more than one terminal sink.
    #[error("emission sink table must have one terminal sink")]
    MultipleTerminalSinks,
    /// Indicates that the terminal sink is not the final entry in the table.
    #[error("emission terminal sink must be the final sink")]
    TerminalSinkNotFinal,
    /// Indicates that fixed sink percentages exceed 100.
    #[error("emission fixed percentages exceed 100")]
    FixedPercentagesExceed100,
    /// Indicates that the terminal emission sink returned an unused amount.
    #[error("terminal emission sink returned unused amount")]
    TerminalSinkReturnedUnusedAmount,
    /// Indicates that the late-settlement terminal sink returned an unused amount.
    #[error("late-settlement terminal sink returned unused amount")]
    LateSettlementSinkReturnedUnusedAmount,
}

impl From<EmissionLimitError> for PrecompileError {
    fn from(err: EmissionLimitError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}

/// Typed errors for `EmissionDay` storage, linking, and conservation invariants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EmissionError {
    #[error("EmissionDay not found")]
    EmissionDayNotFound,
    #[error("invalid emission day key {day}")]
    InvalidEmissionDay { day: u32 },
    #[error("conflicting WWD link on emission day {day}: existing {existing_wwd}, attempted {attempted_wwd}")]
    ConflictingWwdLink {
        day: u32,
        existing_wwd: u32,
        attempted_wwd: u32,
    },
    #[error("emission day {day} is already allocated (state={state})")]
    DayAlreadyAllocated { day: u32, state: u8 },
    #[error("invalid EmissionDay state transition on day {day}: {from_state} -> {to_state}")]
    InvalidStateTransition {
        day: u32,
        from_state: u8,
        to_state: u8,
    },
    #[error(
        "invalid EmissionDay metadosis_state transition on day {day}: {from_state} -> {to_state}"
    )]
    InvalidMetadosisStateTransition {
        day: u32,
        from_state: u8,
        to_state: u8,
    },
    #[error("emission day amount overflow (exceeds u128)")]
    AmountOverflow,
    #[error("emission day {day} conservation invariant violated: expected {expected_limit}, landed {landed_sum}")]
    ConservationInvariantViolated {
        day: u32,
        expected_limit: u128,
        landed_sum: u128,
    },
    #[error("metadosis_start waiting window overflow")]
    WaitingWindowOverflow,
    #[error("date step error ({kind}) for date key {date_key}")]
    DateStep { kind: u8, date_key: u32 },
}

impl From<TimeError> for EmissionError {
    fn from(err: TimeError) -> Self {
        match err.date_step() {
            Some((kind, date_key)) => Self::DateStep { kind, date_key },
            None => Self::InvalidEmissionDay { day: 0 },
        }
    }
}

impl From<EmissionError> for PrecompileError {
    fn from(err: EmissionError) -> Self {
        let bytes = match err {
            EmissionError::EmissionDayNotFound => IEmission::EmissionDayNotFound {}.abi_encode(),
            EmissionError::InvalidEmissionDay { day } => {
                IEmission::InvalidEmissionDay { day }.abi_encode()
            }
            EmissionError::ConflictingWwdLink {
                day,
                existing_wwd,
                attempted_wwd,
            } => IEmission::ConflictingWwdLink {
                day,
                existingWwd: existing_wwd,
                attemptedWwd: attempted_wwd,
            }
            .abi_encode(),
            EmissionError::DayAlreadyAllocated { day, state } => {
                IEmission::DayAlreadyAllocated { day, state }.abi_encode()
            }
            EmissionError::InvalidStateTransition {
                day,
                from_state,
                to_state,
            } => IEmission::InvalidStateTransition {
                day,
                fromState: from_state,
                toState: to_state,
            }
            .abi_encode(),
            EmissionError::InvalidMetadosisStateTransition {
                day,
                from_state,
                to_state,
            } => IEmission::InvalidMetadosisStateTransition {
                day,
                fromState: from_state,
                toState: to_state,
            }
            .abi_encode(),
            EmissionError::AmountOverflow => IEmission::AmountOverflow {}.abi_encode(),
            EmissionError::ConservationInvariantViolated {
                day,
                expected_limit,
                landed_sum,
            } => IEmission::ConservationInvariantViolated {
                day,
                expectedLimit: expected_limit,
                landedSum: landed_sum,
            }
            .abi_encode(),
            EmissionError::WaitingWindowOverflow => {
                IEmission::WaitingWindowOverflow {}.abi_encode()
            }
            EmissionError::DateStep { kind, date_key } => IEmission::DateStep {
                kind,
                dateKey: date_key,
            }
            .abi_encode(),
        };
        PrecompileError::RevertBytes(bytes.into())
    }
}
