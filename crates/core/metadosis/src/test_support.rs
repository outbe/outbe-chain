//! Native predecessor fixtures for cross-crate Metadosis tests.
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::WorldwideDay;

pub fn fresh_devnet_sentinel_is_pristine(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
) -> Result<bool> {
    Ok(
        outbe_worldwideday::api::worldwide_days(storage.clone())?.is_empty()
            && outbe_emission::api::get_emission_day(storage, day.value())?.is_none(),
    )
}
