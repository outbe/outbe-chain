//! Native predecessor fixtures for cross-crate Metadosis tests.
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};

/// Seeds the bootstrap boundary for cross-crate tests that classify by it,
/// without creating a genesis WorldwideDay.
pub fn seed_bootstrap_end_time(storage: StorageHandle<'_>, end_time: u64) -> Result<()> {
    crate::schema::MetadosisContract::new(storage)
        .bootstrap_end_time
        .write(end_time)
}

pub fn fresh_devnet_sentinel_is_pristine(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
) -> Result<bool> {
    let contract = crate::schema::MetadosisContract::new(storage.clone());
    Ok(crate::api::worldwide_days(storage)?.is_empty()
        && contract.emission_days.get(day.value())?.is_none())
}
