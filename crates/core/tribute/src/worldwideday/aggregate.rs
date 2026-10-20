use std::collections::{BTreeMap, BTreeSet};

use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::precompile::ITribute;
use crate::schema::TributeContract;

use super::{
    constants::MAX_ACTIVE_WWDS,
    errors::WwdStorageCorruptionError,
    schema::WorldwideDay as WorldwideDayRecord,
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
}

impl WwdProjection {
    pub(crate) const fn from_record(worldwide_day: WorldwideDay, record: &WorldwideDayRecord) -> Self {
        Self {
            worldwide_day,
            day: record.day,
            forming_start: record.forming_start,
            forming_end: record.forming_end,
            offerings_start: record.offerings_start,
            offering_end: record.offering_end,
        }
    }

    #[must_use]
    pub const fn is_offering_at(&self, timestamp: u64) -> bool {
        timestamp >= self.offerings_start && timestamp < self.offering_end
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
        let contract = TributeContract::new(storage);
        let active_set = collect_unique("active", contract.active_wwd.read_all()?)?;
        if active_set.len() > MAX_ACTIVE_WWDS {
            return Err(
                WwdStorageCorruptionError::ActiveWwdPopulationExceedsDerivedCap {
                    count: active_set.len(),
                    max_active_wwds: MAX_ACTIVE_WWDS,
                }
                .into(),
            );
        }
        let mut records = BTreeMap::new();
        for wwd in &active_set {
            let record = contract.worldwide_days.get(*wwd)?.ok_or(
                WwdStorageCorruptionError::ActiveIndexPointsToMissingWwd { wwd: *wwd },
            )?;
            validate_record_shape(*wwd, &record)?;
            records.insert(*wwd, WwdProjection::from_record(*wwd, &record));
        }
        let active_order: Vec<WorldwideDay> = active_set.into_iter().collect();
        Ok(Self {
            records,
            active_order,
        })
    }
}

pub fn create_worldwide_day(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
    schedule: NewWwdSchedule,
) -> Result<bool> {
    if let Some(existing) = tribute.worldwide_days.get(worldwide_day)? {
        if existing.day != schedule.day {
            return Err(WwdStorageCorruptionError::WwdDayLinkMismatch {
                wwd: worldwide_day,
                expected_day: schedule.day,
                found_day: existing.day,
            }
            .into());
        }
        return Ok(false);
    }

    let record = WorldwideDayRecord {
        wwd: worldwide_day,
        day: schedule.day,
        forming_start: schedule.forming_start,
        forming_end: schedule.forming_end,
        offerings_start: schedule.offerings_start,
        offering_end: schedule.offering_end,
    };
    validate_record_shape(worldwide_day, &record)?;

    let active = tribute.active_wwd.read_all()?;
    if active.len() >= MAX_ACTIVE_WWDS {
        return Err(WwdStorageCorruptionError::ActiveWwdCapReached {
            max_active_wwds: MAX_ACTIVE_WWDS,
            wwd: worldwide_day,
        }
        .into());
    }

    tribute.worldwide_days.create(&record)?;
    tribute.active_wwd.insert(worldwide_day)?;
    tribute.seal_day(worldwide_day)?;
    tribute.emit(ITribute::WorldwideDayStarted {
        worldwideDay: worldwide_day.into(),
        day: record.day,
        formingStart: record.forming_start,
        formingEnd: record.forming_end,
        offeringsStart: record.offerings_start,
        offeringEnd: record.offering_end,
    })?;
    Ok(true)
}

pub fn remove_active_worldwide_day(
    tribute: &mut TributeContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    tribute.active_wwd.remove(&worldwide_day)?;
    Ok(())
}

fn validate_record_shape(wwd: WorldwideDay, record: &WorldwideDayRecord) -> Result<()> {
    if ![
        record.forming_start,
        record.forming_end,
        record.offerings_start,
        record.offering_end,
    ]
    .windows(2)
    .all(|pair| pair[0] <= pair[1])
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
