use outbe_macros::contract;
use outbe_primitives::addresses::{ACCOUNTING_PROGRESS_ADDRESS, CYCLE_ADDRESS};
use outbe_primitives::storage::types::{Mapping, Slot};

/// EVM storage layout for the Cycle dispatcher.
///
/// Tracks per-trigger execution state: the last recorded timestamp and the
/// block number that wrote it. `last_executed_at` is not always a slot value.
/// The first encounter stores `block.timestamp`.
/// A coalescing trigger stores `last_fire_at`, the latest due slot.
/// A non-coalescing trigger stores `next_fire_at`, the earliest missed slot.
///
/// Storage slots:
///   0:  last_executed_at             - mapping(uint32 => uint64)
///   1:  last_executed_block_number   - mapping(uint32 => uint64)
///   2:  active_utc_day                - uint32
#[contract(addr = CYCLE_ADDRESS)]
pub struct Cycle {
    /// Per-trigger timestamp of the last dispatch record.
    /// The first encounter stores `block.timestamp`.
    /// A coalescing trigger stores the latest due slot (`last_fire_at`).
    /// A non-coalescing trigger stores the earliest missed slot
    /// (`next_fire_at`). [`crate::triggers::next_fire_at`] reads this value.
    pub last_executed_at: Mapping<u32, u64>,

    /// Per-trigger block number that last fired the trigger. It is useful for
    /// auditing which block dispatched which slot. The scheduling math does not
    /// consult it.
    pub last_executed_block_number: Mapping<u32, u64>,

    /// UTC calendar day currently owned by ProtocolCycle. Genesis seeds this
    /// from the consensus header timestamp. A contiguous transition settles it.
    /// A multi-day halt advances it without synthesizing missed-day economics.
    pub active_utc_day: Slot<u32>,
}

/// V2 Phase 1 accounting-progress storage facade (`ACCOUNTING_PROGRESS_ADDRESS`).
///
/// ```text
/// slot  0: last_accounted_block_number - u64
/// slot  1..=15: reserved (genesis zero; do not reuse)
/// ```
///
/// Slot 0 is the highest exact-parent block number whose V2 Phase 1 system
/// tx has successfully committed. Cycle reads this slot through
/// [`crate::state::read_last_accounted_block_number`] to gate per-day
/// accounting. The begin-block finalization path reads it for per-block
/// accounting.
#[contract(addr = ACCOUNTING_PROGRESS_ADDRESS)]
pub struct Accounting {
    /// Highest exact-parent block number whose V2 Phase 1 system tx has
    /// successfully committed. `0` means no Phase 1 has committed yet on
    /// this chain (fresh genesis).
    pub last_accounted_block_number: Slot<u64>,
}
