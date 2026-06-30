use alloy_primitives::U256;
use outbe_common::WorldwideDay as WorldwideDayKey;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::error::PrecompileError;

/// WorldwideDay lifecycle status — the **single source of truth** for the status
/// space. Stored on-chain as `u8` (the discriminants are the stored values). The
/// [`status`] module re-exports those values as `u8` constants (for storage
/// defaults and cross-crate comparisons); they are *derived* from this enum and
/// therefore cannot drift from it.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Forming = 0,
    LookbackDelay = 1,
    Offering = 2,
    Waiting = 3,
    Ready = 4,
    InProgress = 5,
    Completed = 6,
    Failed = 7,
}

impl Status {
    /// Only these statuses advance on a clock tick (the rest are settlement).
    pub fn is_time_driven(self) -> bool {
        matches!(
            self,
            Status::Forming | Status::LookbackDelay | Status::Offering | Status::Waiting
        )
    }

    /// Human-readable label (for event payloads) — the single source of the status
    /// strings that settlement events report.
    pub fn label(self) -> &'static str {
        match self {
            Status::Forming => "FORMING",
            Status::LookbackDelay => "LOOKBACK_DELAY",
            Status::Offering => "OFFERING",
            Status::Waiting => "WAITING",
            Status::Ready => "READY",
            Status::InProgress => "IN_PROGRESS",
            Status::Completed => "COMPLETED",
            Status::Failed => "FAILED",
        }
    }
}

impl TryFrom<u8> for Status {
    type Error = PrecompileError;

    fn try_from(v: u8) -> Result<Self, PrecompileError> {
        use Status::*;
        Ok(match v {
            0 => Forming,
            1 => LookbackDelay,
            2 => Offering,
            3 => Waiting,
            4 => Ready,
            5 => InProgress,
            6 => Completed,
            7 => Failed,
            other => {
                return Err(PrecompileError::Revert(format!(
                    "bad worldwide day status {other}"
                )))
            }
        })
    }
}

/// WorldwideDay status values stored as u8, derived from [`Status`] (the single
/// owner) so the constants cannot drift from the enum.
pub mod status {
    use super::Status;
    pub const FORMING: u8 = Status::Forming as u8;
    pub const LOOKBACK_DELAY: u8 = Status::LookbackDelay as u8;
    pub const OFFERING: u8 = Status::Offering as u8;
    pub const WAITING: u8 = Status::Waiting as u8;
    pub const READY: u8 = Status::Ready as u8;
    pub const IN_PROGRESS: u8 = Status::InProgress as u8;
    pub const COMPLETED: u8 = Status::Completed as u8;
    pub const FAILED: u8 = Status::Failed as u8;
}

/// WorldwideDay rate type — the **single owner** of the day-type space (mirrors
/// [`Status`]). Stored on-chain as `u8`; the [`day_type`] module re-exports the
/// values as `u8` constants (storage defaults + cross-crate comparisons), derived
/// from this enum so they cannot drift.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayType {
    Unknown = 0,
    Green = 1,
    Red = 2,
}

impl DayType {
    /// Human-readable label (for event payloads).
    pub fn label(self) -> &'static str {
        match self {
            DayType::Green => "GREEN",
            DayType::Red => "RED",
            DayType::Unknown => "UNKNOWN",
        }
    }
}

impl TryFrom<u8> for DayType {
    type Error = PrecompileError;

    fn try_from(v: u8) -> Result<Self, PrecompileError> {
        Ok(match v {
            0 => DayType::Unknown,
            1 => DayType::Green,
            2 => DayType::Red,
            other => {
                return Err(PrecompileError::Revert(format!(
                    "bad worldwide day type {other}"
                )))
            }
        })
    }
}

/// Day type values stored as u8, derived from [`DayType`] (the single owner) so
/// the constants cannot drift from the enum.
pub mod day_type {
    use super::DayType;
    pub const UNKNOWN: u8 = DayType::Unknown as u8;
    pub const GREEN: u8 = DayType::Green as u8;
    pub const RED: u8 = DayType::Red as u8;
}

#[storage_record(exists_field = forming_start)]
pub struct WorldwideDay {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0, default = status::FORMING)]
    pub status: u8,

    #[attribute(order = 1, default = day_type::UNKNOWN)]
    pub day_type: u8,

    #[attribute(order = 2)]
    pub forming_start: u64,

    #[attribute(order = 3)]
    pub forming_end: u64,

    #[attribute(order = 4)]
    pub lookback_end: u64,

    #[attribute(order = 5)]
    pub offering_end: u64,

    #[attribute(order = 6)]
    pub scheduled_process_time: u64,

    #[attribute(order = 7, default = U256::ZERO)]
    pub metadosis_limit_amount: U256,

    #[attribute(order = 8, default = U256::ZERO)]
    pub previous_vwap: U256,

    #[attribute(order = 9, default = U256::ZERO)]
    pub current_vwap: U256,
}

/// EVM storage layout for the Metadosis orchestrator contract.
///
/// Manages worldwide day lifecycle and daily emission accumulation.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {
    #[attribute(order = 0)]
    pub bootstrap_end_time: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 1)]
    pub worldwide_days: outbe_primitives::storage::dsl::Map<WorldwideDayKey, WorldwideDay>,

    /// Active (non-terminal) WorldwideDay membership. The Set carries its own
    /// length slot, so this is the sole source of the active-day count — there is
    /// no separate counter field.
    #[attribute(order = 2)]
    pub active_wwd: outbe_primitives::storage::dsl::Set<WorldwideDayKey>,

    /// Bounded FIFO of terminal (COMPLETED/FAILED) WorldwideDays, newest at the
    /// back. Capped at `MAX_RECORDS_KEPT`: when a new terminal day pushes past
    /// the cap, the oldest is popped from the front and its record deleted.
    #[attribute(order = 3)]
    pub closed_wwd: outbe_primitives::storage::dsl::Deque<WorldwideDayKey>,
}
