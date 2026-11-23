use alloy_primitives::B256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::storage::types::Mapping;
use outbe_system_common::addresses::TRIBUTE_FACTORY_ADDRESS;

/// Number of recent SU hashes retained in the replay-guard ring.
///
/// An SU id is valid across the WorldwideDay pipeline horizon, from the forming
/// start at 10:00 D-1 to the offering end at 12:00 D+2 (74h). At about
/// 50,000 SU hashes per day, `1 << 20` (`1_048_576`) entries cover about 21
/// days of volume before eviction. A change to this value is a hard fork.
pub const SU_HASH_RETAIN: u64 = 1 << 20;

/// EVM storage layout for the Tribute Factory contract.
///
/// Tracks used SU (Spending Unit) hashes to prevent replay.
#[storage_schema]
#[contract(addr = TRIBUTE_FACTORY_ADDRESS)]
pub struct TributeFactoryContract {
    // slot 0: used SU hash marker (suHash -> bool)
    #[attribute(order = 0)]
    pub used_su_hashes: Map<B256, bool>,

    // slot 1: bounded prune ring of recent SU hashes (seq % SU_HASH_RETAIN -> suHash)
    #[attribute(order = 1)]
    pub su_hash_ring: Mapping<u64, B256>,

    // slot 2: monotonic write cursor for `su_hash_ring`
    #[attribute(order = 2)]
    pub su_hash_ring_seq: Value<u64>,
}
