use outbe_macros::contract;
use outbe_primitives::storage::types::Mapping;
use outbe_system_common::addresses::CYCLE_ADDRESS;

pub use outbe_rewards::accounting::Accounting;

/// EVM storage layout for the Cycle dispatcher.
///
/// Tracks per-task execution state: the last recorded timestamp and the
/// block number that wrote it. `last_executed_at` is not always a slot value.
/// The first encounter stores `block.timestamp`.
/// A coalescing task stores `last_fire_at`, the latest due slot.
/// A non-coalescing task stores `next_fire_at`, the earliest missed slot.
///
/// Storage slots:
///   0:  last_executed_at             - mapping(uint32 => uint64)
///   1:  last_executed_block_number   - mapping(uint32 => uint64)
#[contract(addr = CYCLE_ADDRESS)]
pub struct Cycle {
    /// Per-task timestamp of the last dispatch record.
    /// The first encounter stores `block.timestamp`.
    /// A coalescing task stores the latest due slot (`last_fire_at`).
    /// A non-coalescing task stores the earliest missed slot
    /// (`next_fire_at`). [`crate::tasks::next_fire_at`] reads this value.
    pub last_executed_at: Mapping<u32, u64>,

    /// Per-task block number that last fired the task. It is useful for
    /// auditing which block dispatched which slot. The scheduling math does not
    /// consult it.
    pub last_executed_block_number: Mapping<u32, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn storage_slot_layout_is_pinned() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);

        let cycle = Cycle::new(handle.clone());
        assert_eq!(cycle.last_executed_at.base_slot(), U256::from(0));
        assert_eq!(cycle.last_executed_block_number.base_slot(), U256::from(1));

        let accounting = Accounting::new(handle);
        assert_eq!(accounting.last_accounted_block_number.slot(), U256::from(0));
    }
}
