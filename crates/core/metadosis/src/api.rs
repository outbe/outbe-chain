use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{
    aggregate::{ValidatedWwdAggregate, WwdProjection},
    schema::MetadosisContract,
};

pub use crate::aggregate::{WwdDayType, WwdStatus as WorldwideDayStatus};
pub use crate::state::DayLimitFormationReceipt;
pub use crate::terminal::{CapacityForfeitureReceipt, MissedOfferingReceipt};

/// When the bootstrap ends: the moment the first Worldwide Day opens its
/// offering. `None` before that day exists, so callers name what zero means.
pub fn bootstrap_end_time(storage: StorageHandle<'_>) -> Result<Option<u64>> {
    let end = MetadosisContract::new(storage).get_bootstrap_end_time()?;
    Ok((end != 0).then_some(end))
}

/// Reads one typed WWD projection without exposing its storage facade.
pub fn worldwide_day(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<WwdProjection>> {
    outbe_tribute::worldwideday::api::worldwide_day(storage, wwd)
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
    outbe_tribute::worldwideday::api::is_offering_day(storage, wwd)
}

/// Returns indexed OFFERING days as typed keys without exposing status bytes or
/// the storage facade.
pub fn offering_worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WorldwideDay>> {
    outbe_tribute::worldwideday::api::offering_worldwide_days(storage)
}

/// Reads the durable Metadosis-owned semantic result for one Cycle day-limit
/// formation slot.
pub fn day_limit_formation_receipt(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<DayLimitFormationReceipt>> {
    MetadosisContract::new(storage).day_limit_formation_receipt(wwd)
}

/// Reads the typed durable missed-offering result for one terminal WWD.
pub fn missed_offering_receipt(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<MissedOfferingReceipt>> {
    MetadosisContract::new(storage).read_missed_offering_receipt(wwd)
}

/// Reads the typed durable capacity-forfeiture result for one terminal WWD.
pub fn capacity_forfeiture_receipt(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<CapacityForfeitureReceipt>> {
    MetadosisContract::new(storage).read_capacity_forfeiture_receipt(wwd)
}
