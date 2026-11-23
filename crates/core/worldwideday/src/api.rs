use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::{EmissionDate, WorldwideDay};

use crate::precompile::IWorldwideDay;
use crate::schema::WorldwideDayContract;

use super::{
    aggregate::{ValidatedWwdAggregate, WwdProjection},
    errors::WwdStorageCorruptionError,
};

/// Reads one typed WWD projection without exposing its storage facade.
pub fn worldwide_day(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<WwdProjection>> {
    let contract = WorldwideDayContract::new(storage);
    let Some(record) = contract.worldwide_days.get(wwd)? else {
        return Ok(None);
    };
    Ok(Some(WwdProjection::from_record(wwd, &record)?))
}

/// Reads the complete bounded typed WWD aggregate without exposing indexes or
/// storage entries. This function sorts the returned projections by WorldwideDay.
pub fn worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WwdProjection>> {
    Ok(ValidatedWwdAggregate::load_and_validate(storage)?
        .records()
        .copied()
        .collect())
}

/// The emission day of `wwd`, or `None` when the WWD was never opened.
pub fn emission_day_of(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<EmissionDate>> {
    Ok(WorldwideDayContract::new(storage)
        .worldwide_days
        .get(wwd)?
        .map(|record| EmissionDate::new(record.emission_day)))
}

pub fn is_sealed(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<bool> {
    WorldwideDayContract::new(storage).sealed.read(&wwd)
}

/// Seals `wwd` under a Worldwide Day lease.
pub fn seal_worldwide_day(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<()> {
    crate::lease::require_worldwide_day_lease(&storage, wwd)?;
    let mut contract = WorldwideDayContract::new(storage);
    contract.sealed.write(&wwd, true)?;
    contract.emit(IWorldwideDay::WorldwideDaySealed {
        worldwideDay: wwd.into(),
    })
}

/// Returns true when `wwd` exists, is not sealed, and the current block
/// timestamp is within `[offerings_start, offering_end)`.
pub fn is_offering_day(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<bool> {
    let now = u64::try_from(storage.timestamp()?).unwrap_or(u64::MAX);
    let Some(projection) = worldwide_day(storage.clone(), wwd)? else {
        return Ok(false);
    };
    if !projection.is_offering_at(now) {
        return Ok(false);
    }
    Ok(!is_sealed(storage, wwd)?)
}

/// Returns indexed pipeline WWDs whose offering window `[offerings_start, offering_end)`
/// contains the current block timestamp and which are not yet sealed.
pub fn offering_worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WorldwideDay>> {
    let now = u64::try_from(storage.timestamp()?).unwrap_or(u64::MAX);
    let contract = WorldwideDayContract::new(storage.clone());
    let mut result = Vec::new();
    for wwd in contract.pipeline_wwd.read_all()? {
        let record = contract
            .worldwide_days
            .get(wwd)?
            .ok_or(WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd { wwd })?;
        if now >= record.offering_start
            && now < record.offering_end
            && !contract.sealed.read(&wwd)?
        {
            result.push(wwd);
        }
    }
    result.sort();
    Ok(result)
}
