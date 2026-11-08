//! Legacy unit-ceiling stubs retained temporarily until `outbe-chain-iuh.6`
//! deletes `end_gas.rs` and `StorageHandle::run_gas_bounded_unit`.

/// Gas cap for one resumable call or forfeit unit.
pub const CALL_UNIT_GAS_CEILING: u64 = 1_500_000;
/// Gas cap for one cleanup deletion.
pub const CLEANUP_UNIT_GAS_CEILING: u64 = 1_500_000;
/// Gas cap for one completed notice send.
pub const NOTICE_UNIT_GAS_CEILING: u64 = 1_500_000;

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

pub fn unit_does_not_fit(remaining: Option<u64>, kind: UnitKind) -> bool {
    remaining.is_some_and(|left| left < unit_ceiling(kind))
}
