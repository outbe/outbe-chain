use std::collections::{BTreeMap, BTreeSet};

use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_tribute::TributeContract;

use crate::precompile::ITributeFactory;
use crate::schema::TributeFactoryContract;

use super::{
    constants::{MAX_ACTIVE_WWDS, MAX_PIPELINE_WWDS},
    errors::WwdStorageCorruptionError,
    schema::{wwd_state, WorldwideDay as WorldwideDayRecord},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewWwdSchedule {
    pub day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offerings_start: u64,
    pub offering_end: u64,
}

/// Immutable, typed read projection for one persisted WorldwideDay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WwdProjection {
    pub worldwide_day: WorldwideDay,
    pub day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offerings_start: u64,
    pub offering_end: u64,
    pub state: u8,
}

impl WwdProjection {
    pub(crate) fn from_record(
        worldwide_day: WorldwideDay,
        record: &WorldwideDayRecord,
    ) -> Result<Self> {
        Ok(Self {
            worldwide_day,
            day: record.emission_day,
            forming_start: record
                .forming_start()
                .map_err(super::errors::date_step_error)?,
            forming_end: record
                .forming_end()
                .map_err(super::errors::date_step_error)?,
            offerings_start: record.offering_start,
            offering_end: record.offering_end,
            state: record.state,
        })
    }

    #[must_use]
    pub const fn is_offering_at(&self, timestamp: u64) -> bool {
        self.state == wwd_state::ACTIVE
            && timestamp >= self.offerings_start
            && timestamp < self.offering_end
    }
}

/// Complete bounded snapshot of indexed WWD state.
#[derive(Debug)]
pub struct ValidatedWwdAggregate {
    records: BTreeMap<WorldwideDay, WwdProjection>,
    active_order: Vec<WorldwideDay>,
}

impl ValidatedWwdAggregate {
    pub fn record(&self, worldwide_day: WorldwideDay) -> Option<&WwdProjection> {
        self.records.get(&worldwide_day)
    }

    pub fn active_records(&self) -> impl Iterator<Item = &WwdProjection> {
        self.active_order
            .iter()
            .map(|wwd| self.records.get(wwd).expect("validated active order"))
    }

    pub fn records(&self) -> impl Iterator<Item = &WwdProjection> {
        self.records.values()
    }

    pub fn load_and_validate(storage: StorageHandle<'_>) -> Result<Self> {
        let contract = TributeFactoryContract::new(storage);
        let active_set = collect_unique("active", contract.active_wwd.read_all()?)?;
        let pipeline_set = collect_unique("pipeline", contract.pipeline_wwd.read_all()?)?;
        let mut combined_set = pipeline_set;
        combined_set.extend(active_set);
        if combined_set.len() > MAX_ACTIVE_WWDS {
            return Err(
                WwdStorageCorruptionError::ActiveWwdPopulationExceedsDerivedCap {
                    count: combined_set.len(),
                    max_active_wwds: MAX_ACTIVE_WWDS,
                }
                .into(),
            );
        }
        let mut records = BTreeMap::new();
        for wwd in &combined_set {
            let record = contract
                .worldwide_days
                .get(*wwd)?
                .ok_or(WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd { wwd: *wwd })?;
            validate_record_shape(*wwd, &record)?;
            records.insert(*wwd, WwdProjection::from_record(*wwd, &record)?);
        }
        let active_order: Vec<WorldwideDay> = combined_set.into_iter().collect();
        Ok(Self {
            records,
            active_order,
        })
    }
}

pub fn create_worldwide_day(
    factory: &mut TributeFactoryContract<'_>,
    worldwide_day: WorldwideDay,
    schedule: NewWwdSchedule,
) -> Result<bool> {
    outbe_tribute::runtime::require_worldwide_day_creation_lease(&factory.storage, worldwide_day)?;
    if let Some(existing) = factory.worldwide_days.get(worldwide_day)? {
        if existing.emission_day != schedule.day {
            return Err(WwdStorageCorruptionError::WwdDayLinkMismatch {
                wwd: worldwide_day,
                expected_day: schedule.day,
                found_day: existing.emission_day,
            }
            .into());
        }
        return Ok(false);
    }

    validate_schedule_shape(worldwide_day, &schedule)?;
    let now = u64::try_from(factory.storage.timestamp()?).unwrap_or(0);
    let is_historical_closed = now > 0 && schedule.offering_end <= now;
    let record = WorldwideDayRecord {
        wwd: worldwide_day,
        emission_day: schedule.day,
        offering_start: schedule.offerings_start,
        offering_end: schedule.offering_end,
        state: if is_historical_closed {
            wwd_state::CLOSED
        } else {
            wwd_state::PENDING
        },
    };
    validate_record_shape(worldwide_day, &record)?;

    if is_historical_closed {
        factory.worldwide_days.create(&record)?;
        let mut tribute = TributeContract::new(factory.storage.clone());
        if !tribute.is_day_sealed(worldwide_day)? {
            tribute.seal_day(worldwide_day)?;
        }
    } else {
        let pipeline = factory.pipeline_wwd.read_all()?;
        if pipeline.len() >= MAX_PIPELINE_WWDS {
            return Err(WwdStorageCorruptionError::ActiveWwdCapReached {
                max_active_wwds: MAX_PIPELINE_WWDS,
                wwd: worldwide_day,
            }
            .into());
        }
        factory.worldwide_days.create(&record)?;
        factory.pipeline_wwd.insert(worldwide_day)?;
    }

    factory.emit(ITributeFactory::WorldwideDayStarted {
        worldwideDay: worldwide_day.into(),
        day: record.emission_day,
        formingStart: record
            .forming_start()
            .map_err(super::errors::date_step_error)?,
        formingEnd: record
            .forming_end()
            .map_err(super::errors::date_step_error)?,
        offeringsStart: record.offering_start,
        offeringEnd: record.offering_end,
    })?;
    Ok(true)
}

pub fn activate_worldwide_day(
    factory: &mut TributeFactoryContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    let mut record = factory
        .worldwide_days
        .get(worldwide_day)?
        .ok_or(WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd { wwd: worldwide_day })?;
    if record.state != wwd_state::ACTIVE {
        record.state = wwd_state::ACTIVE;
        factory.worldwide_days.update(&record)?;
    }
    factory.active_wwd.insert(worldwide_day)?;
    factory.pipeline_wwd.insert(worldwide_day)?;
    Ok(())
}

pub fn close_worldwide_day(
    factory: &mut TributeFactoryContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    if let Some(mut record) = factory.worldwide_days.get(worldwide_day)? {
        if record.state != wwd_state::CLOSED {
            record.state = wwd_state::CLOSED;
            factory.worldwide_days.update(&record)?;
        }
    }
    factory.active_wwd.remove(&worldwide_day)?;
    factory.pipeline_wwd.remove(&worldwide_day)?;
    let mut tribute = TributeContract::new(factory.storage.clone());
    if !tribute.is_day_sealed(worldwide_day)? {
        tribute.seal_day(worldwide_day)?;
    }
    Ok(())
}

pub fn remove_active_worldwide_day(
    factory: &mut TributeFactoryContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    outbe_tribute::runtime::require_worldwide_day_removal_lease(&factory.storage)?;
    close_worldwide_day(factory, worldwide_day)?;
    Ok(())
}

fn validate_schedule_shape(wwd: WorldwideDay, schedule: &NewWwdSchedule) -> Result<()> {
    if schedule.offerings_start == 0
        || ![
            schedule.forming_start,
            schedule.forming_end,
            schedule.offerings_start,
            schedule.offering_end,
        ]
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
    {
        return Err(WwdStorageCorruptionError::WwdNonMonotonicPhaseBoundaries { wwd }.into());
    }
    Ok(())
}

fn validate_record_shape(wwd: WorldwideDay, record: &WorldwideDayRecord) -> Result<()> {
    if record.offering_start == 0
        || record.offering_start > record.offering_end
        || record.state > wwd_state::CLOSED
    {
        return Err(WwdStorageCorruptionError::WwdNonMonotonicPhaseBoundaries { wwd }.into());
    }
    Ok(())
}

fn collect_unique<I>(label: &'static str, values: I) -> Result<BTreeSet<WorldwideDay>>
where
    I: IntoIterator<Item = WorldwideDay>,
{
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(
                WwdStorageCorruptionError::WwdIndexContainsDuplicate { label, value }.into(),
            );
        }
    }
    Ok(unique)
}
