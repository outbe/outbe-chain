use std::collections::{BTreeMap, BTreeSet};

use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::WorldwideDay;

use crate::precompile::IWorldwideDay;
use crate::schema::WorldwideDayContract;

use super::{
    constants::{MAX_ACTIVE_WWDS, MAX_PIPELINE_WWDS},
    errors::WwdStorageCorruptionError,
    record::WorldwideDay as WorldwideDayRecord,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NewWwdSchedule {
    pub emission_day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offering_start: u64,
    pub offering_end: u64,
}

/// Complete bounded snapshot of indexed WWD state.
#[derive(Debug)]
pub struct ValidatedWwdAggregate {
    records: BTreeMap<WorldwideDay, WorldwideDayRecord>,
    active_order: Vec<WorldwideDay>,
}

impl ValidatedWwdAggregate {
    pub fn record(&self, worldwide_day: WorldwideDay) -> Option<&WorldwideDayRecord> {
        self.records.get(&worldwide_day)
    }

    pub fn active_records(&self) -> impl Iterator<Item = &WorldwideDayRecord> {
        self.active_order
            .iter()
            .map(|wwd| self.records.get(wwd).expect("validated active order"))
    }

    pub fn records(&self) -> impl Iterator<Item = &WorldwideDayRecord> {
        self.records.values()
    }

    pub fn load_and_validate(storage: StorageHandle<'_>) -> Result<Self> {
        let contract = WorldwideDayContract::new(storage);
        let pipeline_set = collect_unique("pipeline", contract.pipeline_wwd.read_all()?)?;
        if pipeline_set.len() > MAX_ACTIVE_WWDS {
            return Err(
                WwdStorageCorruptionError::ActiveWwdPopulationExceedsDerivedCap {
                    count: pipeline_set.len(),
                    max_active_wwds: MAX_ACTIVE_WWDS,
                }
                .into(),
            );
        }
        let mut records = BTreeMap::new();
        for wwd in &pipeline_set {
            let record = contract
                .worldwide_days
                .get(*wwd)?
                .ok_or(WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd { wwd: *wwd })?;
            validate_record_shape(*wwd, &record)?;
            // The forming window of a pipeline WWD must have a valid date.
            record
                .forming_end()
                .map_err(super::errors::date_step_error)?;
            records.insert(*wwd, record);
        }
        let active_order: Vec<WorldwideDay> = pipeline_set.into_iter().collect();
        Ok(Self {
            records,
            active_order,
        })
    }
}

pub fn open_worldwide_day(
    factory: &mut WorldwideDayContract<'_>,
    worldwide_day: WorldwideDay,
    schedule: NewWwdSchedule,
) -> Result<bool> {
    crate::lease::require_worldwide_day_lease(&factory.storage)?;
    if let Some(existing) = factory.worldwide_days.get(worldwide_day)? {
        if existing.emission_day != schedule.emission_day {
            return Err(WwdStorageCorruptionError::WwdDayLinkMismatch {
                wwd: worldwide_day,
                expected_day: schedule.emission_day,
                found_day: existing.emission_day,
            }
            .into());
        }
        return Ok(false);
    }

    validate_schedule_shape(worldwide_day, &schedule)?;
    let record = WorldwideDayRecord {
        wwd: worldwide_day,
        emission_day: schedule.emission_day,
        offering_start: schedule.offering_start,
        offering_end: schedule.offering_end,
    };
    validate_record_shape(worldwide_day, &record)?;

    let pipeline = factory.pipeline_wwd.read_all()?;
    ensure_pipeline_capacity(&pipeline, worldwide_day)?;
    factory.worldwide_days.create(&record)?;
    factory.pipeline_wwd.insert(worldwide_day)?;

    factory.emit(IWorldwideDay::WorldwideDayOpened {
        worldwideDay: worldwide_day.into(),
        formingStart: record
            .forming_start()
            .map_err(super::errors::date_step_error)?,
        formingEnd: record
            .forming_end()
            .map_err(super::errors::date_step_error)?,
        offeringStart: record.offering_start,
        offeringEnd: record.offering_end,
    })?;
    Ok(true)
}

fn ensure_pipeline_capacity(pipeline: &[WorldwideDay], worldwide_day: WorldwideDay) -> Result<()> {
    if pipeline.len() >= MAX_PIPELINE_WWDS {
        return Err(WwdStorageCorruptionError::ActiveWwdCapReached {
            max_active_wwds: MAX_PIPELINE_WWDS,
            wwd: worldwide_day,
        }
        .into());
    }
    Ok(())
}

pub fn close_worldwide_day(
    factory: &mut WorldwideDayContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    factory.pipeline_wwd.remove(&worldwide_day)?;
    if !factory.sealed.read(&worldwide_day)? {
        crate::api::seal_worldwide_day(factory.storage.clone(), worldwide_day)?;
    }
    Ok(())
}

fn validate_schedule_shape(wwd: WorldwideDay, schedule: &NewWwdSchedule) -> Result<()> {
    let ordered = |times: [u64; 3]| times.windows(2).all(|pair| pair[0] <= pair[1]);
    let forming = [
        schedule.forming_start,
        schedule.forming_end,
        schedule.offering_end,
    ];
    let offering = [
        schedule.forming_start,
        schedule.offering_start,
        schedule.offering_end,
    ];
    if schedule.offering_start == 0 || !ordered(forming) || !ordered(offering) {
        return Err(WwdStorageCorruptionError::WwdNonMonotonicPhaseBoundaries { wwd }.into());
    }
    Ok(())
}

fn validate_record_shape(wwd: WorldwideDay, record: &WorldwideDayRecord) -> Result<()> {
    if record.offering_start == 0 || record.offering_start > record.offering_end {
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
