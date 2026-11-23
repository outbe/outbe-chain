use outbe_macros::{contract, storage_schema};
use outbe_system_common::addresses::WORLDWIDE_DAY_ADDRESS;
use outbe_system_common::time::WorldwideDay;

/// EVM storage layout of the Worldwide Day precompile.
#[storage_schema]
#[contract(addr = WORLDWIDE_DAY_ADDRESS)]
pub struct WorldwideDayContract {
    #[attribute(order = 0)]
    pub worldwide_days: Map<WorldwideDay, crate::record::WorldwideDay>,

    /// Unsealed WorldwideDays of the pipeline.
    #[attribute(order = 1)]
    pub pipeline_wwd: Set<WorldwideDay>,

    /// Last UTC calendar day (YYYYMMDD) on which the daily WWD step succeeded.
    #[attribute(order = 2)]
    pub last_wwd_advance_day: Value<u32>,

    #[attribute(order = 3)]
    pub sealed: Map<WorldwideDay, bool>,
}
