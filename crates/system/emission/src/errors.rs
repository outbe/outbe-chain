//! Error types for the Emission system precompile (`outbe-emission`).

use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_system_common::time::TimeError;
use thiserror::Error;

use crate::precompile::IEmission;

/// Represents errors returned by the Emission allocation and ledger runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EmissionLimitError {
    /// Indicates that fixed-share multiplication overflowed.
    #[error("emission fixed-share multiplication overflow")]
    FixedShareMultiplicationOverflow,
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
    #[error("emission day {day} already has a record")]
    EmissionDayExists { day: u32 },
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
    #[error("invalid EmissionDay desis_state transition on day {day}: {from_state} -> {to_state}")]
    InvalidDesisStateTransition {
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
        landed_sum: i128,
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
            None => match err {
                TimeError::PreGenesis { utc_day, .. } => Self::InvalidEmissionDay { day: utc_day },
                _ => Self::InvalidEmissionDay { day: 0 },
            },
        }
    }
}

impl From<EmissionError> for PrecompileError {
    fn from(err: EmissionError) -> Self {
        let bytes = match err {
            EmissionError::ConservationInvariantViolated { .. } => {
                return PrecompileError::FatalWithCause(Box::new(err));
            }
            EmissionError::EmissionDayNotFound => IEmission::EmissionDayNotFound {}.abi_encode(),
            EmissionError::InvalidEmissionDay { day } => {
                IEmission::InvalidEmissionDay { day }.abi_encode()
            }
            EmissionError::EmissionDayExists { day } => {
                IEmission::EmissionDayExists { day }.abi_encode()
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
            EmissionError::InvalidDesisStateTransition {
                day,
                from_state,
                to_state,
            } => IEmission::InvalidDesisStateTransition {
                day,
                fromState: from_state,
                toState: to_state,
            }
            .abi_encode(),
            EmissionError::AmountOverflow => IEmission::AmountOverflow {}.abi_encode(),
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
