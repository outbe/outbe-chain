//! Typed fresh-devnet genesis adaptation for cross-crate test harnesses.

use outbe_primitives::time::{timestamp_to_date_key, WorldwideDay};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::TributeContract;

use super::{
    aggregate::ValidatedWwdAggregate, errors::WwdStorageCorruptionError,
    schema::WorldwideDay as WorldwideDayRecord,
};

/// One invariant-valid active WWD included in a fresh-devnet genesis artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenesisWorldwideDay {
    pub worldwide_day: WorldwideDay,
    pub day: u32,
    pub forming_start: u64,
    pub forming_end: u64,
    pub offerings_start: u64,
    pub offering_end: u64,
}

impl GenesisWorldwideDay {
    fn validate(mut self) -> Result<Self> {
        if !self.worldwide_day.is_valid() {
            return Err(WwdStorageCorruptionError::GenesisWwdNotValidCalendarDay.into());
        }
        if ![
            self.forming_start,
            self.forming_end,
            self.offerings_start,
            self.offering_end,
        ]
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
        {
            return Err(WwdStorageCorruptionError::GenesisWwdPhaseOrderInvalid.into());
        }
        if self.day == 0 {
            self.day = timestamp_to_date_key(self.offering_end);
        }
        Ok(self)
    }
}

enum GenesisOperation {
    SeedActive(GenesisWorldwideDay),
    RetimeOffering {
        worldwide_day: WorldwideDay,
        offering_end: u64,
    },
    ClearSingleOffering {
        expected_worldwide_day: WorldwideDay,
    },
}

/// Ordered typed mutations applied while constructing a fresh-devnet genesis.
#[derive(Default)]
pub struct FreshDevnetGenesisBuilder {
    operations: Vec<GenesisOperation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FreshDevnetGenesisReport {
    pub changed: bool,
}

impl FreshDevnetGenesisBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn seed_active_worldwide_day(mut self, day: GenesisWorldwideDay) -> Self {
        self.operations.push(GenesisOperation::SeedActive(day));
        self
    }

    #[must_use]
    pub fn retime_offering_day(mut self, worldwide_day: WorldwideDay, offering_end: u64) -> Self {
        self.operations.push(GenesisOperation::RetimeOffering {
            worldwide_day,
            offering_end,
        });
        self
    }

    #[must_use]
    pub fn clear_single_offering_day(mut self, expected_worldwide_day: WorldwideDay) -> Self {
        self.operations.push(GenesisOperation::ClearSingleOffering {
            expected_worldwide_day,
        });
        self
    }

    pub fn apply(self, storage: StorageHandle<'_>) -> Result<FreshDevnetGenesisReport> {
        if storage.block_number()? > 1 {
            return Err(
                WwdStorageCorruptionError::GenesisBuilderCannotRunAfterPreLaunchBoundary.into(),
            );
        }
        let mut changed = false;
        let contract = TributeContract::new(storage.clone());
        for operation in self.operations {
            let Some(operation_changed) = operation.apply(&contract)? else {
                continue;
            };
            changed |= operation_changed;
            ValidatedWwdAggregate::load_and_validate(storage.clone())?;
        }
        Ok(FreshDevnetGenesisReport { changed })
    }
}

impl GenesisOperation {
    fn apply(self, contract: &TributeContract<'_>) -> Result<Option<bool>> {
        match self {
            Self::SeedActive(day) => Self::seed(contract, day),
            Self::RetimeOffering {
                worldwide_day,
                offering_end,
            } => Self::retime(contract, worldwide_day, offering_end),
            Self::ClearSingleOffering {
                expected_worldwide_day,
            } => Self::clear(contract, expected_worldwide_day),
        }
    }

    fn seed(contract: &TributeContract<'_>, day: GenesisWorldwideDay) -> Result<Option<bool>> {
        let day = day.validate()?;
        if contract.worldwide_days.get(day.worldwide_day)?.is_some() {
            return Err(WwdStorageCorruptionError::GenesisWwdAlreadyExists.into());
        }
        contract.worldwide_days.create(&WorldwideDayRecord {
            wwd: day.worldwide_day,
            day: day.day,
            forming_start: day.forming_start,
            forming_end: day.forming_end,
            offerings_start: day.offerings_start,
            offering_end: day.offering_end,
        })?;
        contract.active_wwd.insert(day.worldwide_day)?;
        Ok(Some(true))
    }

    fn retime(
        contract: &TributeContract<'_>,
        worldwide_day: WorldwideDay,
        offering_end: u64,
    ) -> Result<Option<bool>> {
        let mut day = contract
            .worldwide_days
            .get(worldwide_day)?
            .ok_or(WwdStorageCorruptionError::GenesisOfferingWwdMissing)?;
        if day.forming_end > offering_end || day.offerings_start > offering_end {
            return Err(WwdStorageCorruptionError::GenesisWwdPhasesEndAfterOfferingDeadline.into());
        }
        let operation_changed = day.offering_end != offering_end;
        day.offering_end = offering_end;
        contract.worldwide_days.update(&day)?;
        Ok(Some(operation_changed))
    }

    fn clear(
        contract: &TributeContract<'_>,
        expected_worldwide_day: WorldwideDay,
    ) -> Result<Option<bool>> {
        let active = contract.active_wwd.read_all()?;
        if active.len() > 1
            || active
                .first()
                .is_some_and(|day| *day != expected_worldwide_day)
        {
            return Err(WwdStorageCorruptionError::GenesisContainsUnexpectedActiveWwd.into());
        }
        if contract
            .worldwide_days
            .get(expected_worldwide_day)?
            .is_none()
        {
            if active.is_empty() {
                return Ok(None);
            }
            return Err(WwdStorageCorruptionError::GenesisActiveWwdRecordMissing.into());
        }
        if !active.is_empty() {
            contract.active_wwd.remove(&expected_worldwide_day)?;
        }
        contract.worldwide_days.delete(expected_worldwide_day)?;
        Ok(Some(true))
    }
}
