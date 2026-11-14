use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::WorldwideDay;
use outbe_tribute::TributeContract;

use crate::schema::TributeFactoryContract;

use super::{
    aggregate::{ValidatedWwdAggregate, WwdProjection},
    errors::WwdStorageCorruptionError,
};

/// Reads one typed WWD projection without exposing its storage facade.
pub fn worldwide_day(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<WwdProjection>> {
    let contract = TributeFactoryContract::new(storage);
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

/// Returns true when `wwd` exists, has a stored forming VWAP snapshot, is not
/// sealed, and the current block timestamp is within `[offerings_start, offering_end)`.
pub fn is_offering_day(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<bool> {
    let now = u64::try_from(storage.timestamp()?).unwrap_or(u64::MAX);
    let Some(projection) = worldwide_day(storage.clone(), wwd)? else {
        return Ok(false);
    };
    if !projection.is_offering_at(now) {
        return Ok(false);
    }
    if !outbe_oracle::api::has_worldwide_day_vwap_snapshot(storage.clone(), wwd)? {
        return Ok(false);
    }
    let tribute = TributeContract::new(storage);
    Ok(!tribute.is_day_sealed(wwd)?)
}

/// Returns indexed pipeline WWDs whose offering window `[offerings_start, offering_end)`
/// contains the current block timestamp, whose forming price is stored, and which
/// are not yet sealed.
pub fn offering_worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WorldwideDay>> {
    let now = u64::try_from(storage.timestamp()?).unwrap_or(u64::MAX);
    let contract = TributeFactoryContract::new(storage.clone());
    let tribute = TributeContract::new(storage.clone());
    let mut result = Vec::new();
    for wwd in contract.pipeline_wwd.read_all()? {
        let record = contract
            .worldwide_days
            .get(wwd)?
            .ok_or(WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd { wwd })?;
        if now >= record.offering_start
            && now < record.offering_end
            && outbe_oracle::api::has_worldwide_day_vwap_snapshot(storage.clone(), wwd)?
            && !tribute.is_day_sealed(wwd)?
        {
            result.push(wwd);
        }
    }
    result.sort();
    Ok(result)
}
