use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_tribute::worldwideday::{ValidatedWwdAggregate, WwdProjection};

use crate::schema::{EmissionDay, MetadosisContract};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmissionDayProjection {
    pub day: u32,
    pub emission_limit_minor: U256,
    pub wwd: WorldwideDay,
    pub metadosis_start: u64,
    pub state: u8,
}

impl From<EmissionDay> for EmissionDayProjection {
    fn from(record: EmissionDay) -> Self {
        Self {
            day: record.day,
            emission_limit_minor: record.emission_limit_minor,
            wwd: record.wwd,
            metadosis_start: record.metadosis_start,
            state: record.state,
        }
    }
}

/// When the bootstrap ends: the moment the first Worldwide Day opens its
/// offering. `None` before that day exists, so callers name what zero means.
pub fn bootstrap_end_time(storage: StorageHandle<'_>) -> Result<Option<u64>> {
    let end = MetadosisContract::new(storage).bootstrap_end_time.read()?;
    Ok((end != 0).then_some(end))
}

/// Reads one typed EmissionDay projection by its `YYYYMMDD` UTC day key.
pub fn emission_day(
    storage: StorageHandle<'_>,
    day: u32,
) -> Result<Option<EmissionDayProjection>> {
    Ok(MetadosisContract::new(storage)
        .emission_days
        .get(day)?
        .map(EmissionDayProjection::from))
}

/// Reads one typed WWD projection without exposing its storage facade.
pub fn worldwide_day(
    storage: StorageHandle<'_>,
    wwd: WorldwideDay,
) -> Result<Option<WwdProjection>> {
    outbe_tribute::worldwideday::api::worldwide_day(storage, wwd)
}

/// Reads the complete bounded typed WWD aggregate without exposing indexes or
/// storage entries. This function sorts the returned projections by WorldwideDay.
pub fn worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WwdProjection>> {
    Ok(ValidatedWwdAggregate::load_and_validate(storage)?
        .records()
        .copied()
        .collect())
}

/// Checks whether the current block timestamp falls in the offering window of `wwd`.
pub fn is_offering_day(storage: StorageHandle<'_>, wwd: WorldwideDay) -> Result<bool> {
    outbe_tribute::worldwideday::api::is_offering_day(storage, wwd)
}

/// Returns active WorldwideDay keys whose offering window contains the block timestamp.
pub fn offering_worldwide_days(storage: StorageHandle<'_>) -> Result<Vec<WorldwideDay>> {
    outbe_tribute::worldwideday::api::offering_worldwide_days(storage)
}
