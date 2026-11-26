use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::WorldwideDay;

/// Checks whether the current block timestamp falls in the offering window of `wwd`.
pub fn is_offering_day(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<bool> {
    outbe_worldwideday::api::is_offering_day(storage, wwd)
}

/// Returns active WorldwideDay keys whose offering window contains the block timestamp.
pub fn offering_worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WorldwideDay>> {
    outbe_worldwideday::api::offering_worldwide_days(storage)
}
