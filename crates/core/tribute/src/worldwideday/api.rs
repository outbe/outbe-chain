use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::TributeContract;

use super::{
    aggregate::{ValidatedWwdAggregate, WwdMembership, WwdProjection, WwdStatus},
    errors::WwdStorageCorruptionError,
};

pub use super::aggregate::{WwdDayType, WwdStatus as WorldwideDayStatus};

/// Reads one typed WWD projection without exposing its storage facade.
pub fn worldwide_day(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<WwdProjection>> {
    let contract = TributeContract::new(storage);
    let Some(record) = contract.worldwide_days.get(wwd)? else {
        return Ok(None);
    };
    let active = contract.active_wwd.read_all()?.contains(&wwd);
    let closed = contract.closed_wwd.read_all()?.contains(&wwd);
    let membership = match (active, closed) {
        (true, false) => WwdMembership::Active,
        (false, true) => WwdMembership::Closed,
        _ => return Err(WwdStorageCorruptionError::WwdMembershipNotExactlyActiveOrClosed.into()),
    };
    let status = WwdStatus::try_from(record.status)?;
    let day_type = record.day_type.try_into()?;
    Ok(Some(WwdProjection::from_record(
        wwd, status, day_type, membership, &record,
    )))
}

/// Reads the complete bounded typed WWD aggregate without exposing indexes or
/// storage entries. The result is sorted by WorldwideDay.
pub fn worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WwdProjection>> {
    Ok(ValidatedWwdAggregate::load_and_validate(storage)?
        .records()
        .copied()
        .collect())
}

/// Typed query used by TributeFactory instead of a raw status byte read.
pub fn is_offering_day(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<bool> {
    Ok(worldwide_day(storage, wwd)?
        .is_some_and(|projection| projection.status == WwdStatus::Offering))
}

/// Returns indexed OFFERING days as typed keys without exposing status bytes or
/// the storage facade.
pub fn offering_worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WorldwideDay>> {
    let contract = TributeContract::new(storage);
    let mut result = Vec::new();
    for wwd in contract.active_wwd.read_all()? {
        let record = contract
            .worldwide_days
            .get(wwd)?
            .ok_or(WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd)?;
        if WwdStatus::try_from(record.status)? == WwdStatus::Offering {
            result.push(wwd);
        }
    }
    result.sort();
    Ok(result)
}
