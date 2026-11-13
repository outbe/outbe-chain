use alloy_primitives::B256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::{addresses::TRIBUTE_FACTORY_ADDRESS, storage::types::Mapping};
use outbe_system_common::time::WorldwideDay;

/// Number of recent SU hashes retained in the replay-guard ring.
///
/// An SU id is valid across the WorldwideDay pipeline horizon
/// (`E = D + 4` across forming 50h + lookback 22h + offering 50h). At about
/// 50,000 SU hashes per day, `1 << 20` (`1_048_576`) entries cover about 21
/// days of volume before eviction. A change to this value is a hard fork.
pub const SU_HASH_RETAIN: u64 = 1 << 20;

/// EVM storage layout for the Tribute Factory contract.
///
/// Tracks used SU (Spending Unit) hashes to prevent replay, plus the
/// WorldwideDay schedule and active/pipeline index sets.
#[storage_schema]
#[contract(addr = TRIBUTE_FACTORY_ADDRESS)]
pub struct TributeFactoryContract {
    // slot 0: used SU hash marker (suHash -> bool)
    #[attribute(order = 0)]
    pub used_su_hashes: Map<B256, bool>,

    // slot 1: WorldwideDay records (wwd -> 1-slot packed WorldwideDay)
    #[attribute(order = 1)]
    pub worldwide_days: Map<WorldwideDay, crate::worldwideday::schema::WorldwideDay>,

    // slots 2..3: unsealed pipeline WorldwideDays
    #[attribute(order = 2)]
    pub pipeline_wwd: Set<WorldwideDay>,

    // slot 4: one-shot forming VWAP snapshot finalized guard (wwd -> bool)
    #[attribute(order = 3)]
    pub forming_snapshot_finalized: Map<WorldwideDay, bool>,

    // slot 5: bounded prune ring of recent SU hashes (seq % SU_HASH_RETAIN -> suHash)
    #[attribute(order = 4)]
    pub su_hash_ring: Mapping<u64, B256>,

    // slot 6: monotonic write cursor for `su_hash_ring`
    #[attribute(order = 5)]
    pub su_hash_ring_seq: Value<u64>,

    // slot 7: last UTC calendar day (YYYYMMDD) on which the daily WWD step succeeded
    #[attribute(order = 6)]
    pub last_wwd_advance_day: Value<u32>,
}
