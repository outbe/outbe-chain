use alloy_primitives::B256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::{
    addresses::TRIBUTE_FACTORY_ADDRESS, storage::types::Mapping, time::WorldwideDay,
};

/// Number of recent SU hashes whose `used_su_hashes` guards stay live.
///
/// An SU id is valid for about 21 days: forming (50h) + lookback (430h) +
/// offering (50h). At about 50k SU hashes per day, 2^20 entries cover that
/// horizon. After the horizon the SU id has expired, so the ring can clear the
/// guard of its hash. The L2 attestation and the enclave reject a reuse of an
/// expired SU id. A change to this value is a hard fork.
pub const SU_HASH_RETAIN: u64 = 1 << 20;

/// EVM storage layout for the Tribute Factory contract.
///
/// Tracks used SU (Spending Unit) hashes to prevent replay, plus the
/// WorldwideDay schedule and active/pipeline index sets. A prune ring of
/// [`SU_HASH_RETAIN`] entries bounds the used SU hash guards.
#[storage_schema]
#[contract(addr = TRIBUTE_FACTORY_ADDRESS)]
pub struct TributeFactoryContract {
    // slot 0: used SU hash marker (suHash -> bool)
    #[attribute(order = 0)]
    pub used_su_hashes: Map<B256, bool>,

    // slot 1: WorldwideDay records (wwd -> 1-slot packed WorldwideDay)
    #[attribute(order = 1)]
    pub worldwide_days: Map<WorldwideDay, crate::worldwideday::schema::WorldwideDay>,

    // slots 2..3: open offering WorldwideDays (state == Active)
    #[attribute(order = 2)]
    pub active_wwd: Set<WorldwideDay>,

    // slots 4..5: non-closed pipeline WorldwideDays (state == Pending || state == Active)
    #[attribute(order = 3)]
    pub pipeline_wwd: Set<WorldwideDay>,

    // slot 6: one-shot forming VWAP snapshot finalized guard (wwd -> bool)
    #[attribute(order = 4)]
    pub forming_snapshot_finalized: Map<WorldwideDay, bool>,

    // slot 7: prune ring of used SU hashes (cursor % SU_HASH_RETAIN -> suHash).
    // The ring clears the `used_su_hashes` guard of the entry that it replaces.
    // `B256::ZERO` marks an empty entry.
    #[attribute(order = 5)]
    pub su_hash_ring: Mapping<u64, B256>,

    // slot 8: monotonic write cursor of `su_hash_ring`
    #[attribute(order = 6)]
    pub su_hash_ring_seq: Value<u64>,
}
