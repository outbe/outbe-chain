use alloy_primitives::B256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::{addresses::TRIBUTE_FACTORY_ADDRESS, time::WorldwideDay};

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

    // slots 2..3: open offering WorldwideDays (state == Active)
    #[attribute(order = 2)]
    pub active_wwd: Set<WorldwideDay>,

    // slots 4..5: non-closed pipeline WorldwideDays (state == Pending || state == Active)
    #[attribute(order = 3)]
    pub pipeline_wwd: Set<WorldwideDay>,

    // slot 6: one-shot forming VWAP snapshot finalized guard (wwd -> bool)
    #[attribute(order = 4)]
    pub forming_snapshot_finalized: Map<WorldwideDay, bool>,
}
