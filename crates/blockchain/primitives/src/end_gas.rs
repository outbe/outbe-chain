//! Gas stop for the end-zone call and cleanup transactions.
//!
//! Direct callers that do not enter this scope keep the visit and write caps.
//! The ceilings below are the measured cost of one supported unit. A unit that
//! does not fit commits the cursor before it and resumes on the next block.

use std::cell::Cell;

use crate::protocol_schedule::OutbeProtocolSchedule;
use crate::system_tx::{
    system_tx_intrinsic_gas, SystemTx, CALL_EVENT_PROCESSING_SELECTOR, CLEANUP_PROCESSING_SELECTOR,
    SYSTEM_TX_INPUT_VERSION,
};

thread_local! {
    static END_GAS_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Gas cap for one resumable call or forfeit unit.
///
/// The measured storage floor is one warm read plus one reset write (5_100).
/// The cap is larger so one supported unit fits inside the idle end budget
/// after both intrinsics, and a child out-of-gas rolls back only that unit.
pub const CALL_UNIT_GAS_CEILING: u64 = 1_500_000;
/// Gas cap for one cleanup deletion. Same idle-budget bound as a call unit.
pub const CLEANUP_UNIT_GAS_CEILING: u64 = 1_500_000;
/// Gas cap for one completed notice send. Same idle-budget bound as a call unit.
pub const NOTICE_UNIT_GAS_CEILING: u64 = 1_500_000;

pub fn end_gas_active() -> bool {
    END_GAS_ACTIVE.with(|cell| cell.get())
}

pub fn with_end_gas<R>(f: impl FnOnce() -> R) -> R {
    END_GAS_ACTIVE.with(|cell| {
        let previous = cell.replace(true);
        let result = f();
        cell.set(previous);
        result
    })
}

/// Execution gas left for call work on an idle block after both end-zone intrinsics.
pub fn idle_call_execution_gas() -> u64 {
    let reserved = OutbeProtocolSchedule::END_BLOCK_RESERVED_GAS;
    let call_intrinsic = empty_intrinsic(CALL_EVENT_PROCESSING_SELECTOR);
    let cleanup_intrinsic = empty_intrinsic(CLEANUP_PROCESSING_SELECTOR);
    reserved
        .saturating_sub(call_intrinsic)
        .saturating_sub(cleanup_intrinsic)
}

fn empty_intrinsic(selector: [u8; 4]) -> u64 {
    let mut body = Vec::with_capacity(5);
    body.extend_from_slice(&selector);
    body.push(SYSTEM_TX_INPUT_VERSION);
    system_tx_intrinsic_gas(&body).unwrap_or(u64::MAX)
}

pub fn unit_ceiling(kind: UnitKind) -> u64 {
    match kind {
        UnitKind::Call => CALL_UNIT_GAS_CEILING,
        UnitKind::Cleanup => CLEANUP_UNIT_GAS_CEILING,
        UnitKind::Notice => NOTICE_UNIT_GAS_CEILING,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    Call,
    Cleanup,
    Notice,
}

/// True when the next unit must not start. Absence of a meter is not a stop:
/// direct test callers keep the visit and write caps.
pub fn unit_does_not_fit(remaining: Option<u64>, kind: UnitKind) -> bool {
    remaining.is_some_and(|left| left < unit_ceiling(kind))
}

pub fn empty_tx_intrinsic(tx: &SystemTx) -> Option<u64> {
    tx.encode().ok().and_then(|bytes| system_tx_intrinsic_gas(&bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceilings_fit_idle_budget_after_both_intrinsics() {
        let idle = idle_call_execution_gas();
        let measured_storage_floor = 100 + 5_000;
        assert!(CALL_UNIT_GAS_CEILING >= measured_storage_floor);
        assert!(CLEANUP_UNIT_GAS_CEILING >= measured_storage_floor);
        assert!(NOTICE_UNIT_GAS_CEILING >= measured_storage_floor);
        assert!(CALL_UNIT_GAS_CEILING <= idle);
        assert!(CLEANUP_UNIT_GAS_CEILING <= idle);
        assert!(NOTICE_UNIT_GAS_CEILING <= idle);
        assert!(idle > 2_000_000);
    }

    #[test]
    fn scope_restores_the_previous_flag() {
        assert!(!end_gas_active());
        with_end_gas(|| {
            assert!(end_gas_active());
        });
        assert!(!end_gas_active());
    }
}
