//! Native predecessor fixtures for cross-crate Metadosis tests.
use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};

pub fn seed_ready_worldwide_days_for_capacity(
    storage: StorageHandle<'_>,
    worldwide_days: &[WorldwideDay],
) -> Result<()> {
    use crate::errors::MetadosisTestSupportError;
    use crate::fixture_kernel::FixtureKernelExt;

    if worldwide_days.len() > crate::constants::MAX_RETAINED_WWDS {
        return Err(MetadosisTestSupportError::CapacityFixtureExceedsMaxRetainedWwds.into());
    }
    let mut contract = crate::schema::MetadosisContract::new(storage);
    for worldwide_day in worldwide_days {
        if !worldwide_day.is_valid() {
            return Err(MetadosisTestSupportError::CapacityFixtureInvalidWorldwideDay.into());
        }
        contract.fixture_create_ready_day(*worldwide_day, U256::ZERO)?;
        contract.add_active_wwd(*worldwide_day)?;
    }
    Ok(())
}

/// Seeds the bootstrap boundary for cross-crate tests that classify by it,
/// without standing up a genesis Worldwide Day.
pub fn seed_bootstrap_end_time(storage: StorageHandle<'_>, end_time: u64) -> Result<()> {
    crate::schema::MetadosisContract::new(storage).set_bootstrap_end_time(end_time)
}

pub fn fresh_devnet_sentinel_is_pristine(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
) -> Result<bool> {
    let contract = crate::schema::MetadosisContract::new(storage.clone());
    Ok(crate::api::worldwide_days(storage.clone())?.is_empty()
        && contract.read_terminal_receipt(day)?.is_none()
        && crate::api::day_limit_formation_receipt(storage, day)?.is_none())
}
