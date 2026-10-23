//! Native predecessor fixtures for cross-crate Metadosis tests.
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};

pub fn fresh_devnet_sentinel_is_pristine(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
) -> Result<bool> {
    let contract = crate::schema::MetadosisContract::new(storage.clone());
    Ok(crate::api::worldwide_days(storage)?.is_empty()
        && contract.emission_days.get(day.value())?.is_none())
}
