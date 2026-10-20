//! Typed fresh-devnet genesis adaptation for cross-crate test harnesses.
//!
//! The builder is feature-gated, accepts only invariant-valid named operations,
//! and refuses to run after block 1. It exposes no schema entry, slot, provider,
//! mutation permit, or arbitrary callback.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::TributeContract;

use super::{
    aggregate::{ValidatedWwdAggregate, WwdDayType, WwdStatus},
    errors::WwdStorageCorruptionError,
    schema::{status, WorldwideDay as WorldwideDayRecord},
};

/// One invariant-valid active WWD included in a fresh-devnet genesis artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenesisWorldwideDay {
    pub worldwide_day: WorldwideDay,
    pub status: WwdStatus,
    pub day_type: WwdDayType,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
    pub metadosis_limit_minor: U256,
    pub previous_vwap_minor: U256,
    pub current_vwap_minor: U256,
}

impl GenesisWorldwideDay {
    fn validate(self) -> Result<Self> {
        if !self.worldwide_day.is_valid() {
            return Err(WwdStorageCorruptionError::GenesisWwdNotValidCalendarDay.into());
        }
        if !matches!(self.status, WwdStatus::Offering | WwdStatus::Ready) {
            return Err(
                WwdStorageCorruptionError::GenesisBuilderOnlyAcceptsOfferingOrReadyPredecessors
                    .into(),
            );
        }
        if self.day_type == WwdDayType::Unknown {
            return Err(WwdStorageCorruptionError::GenesisWwdRequiresKnownDayType.into());
        }
        if ![
            self.forming_start,
            self.forming_end,
            self.lookback_end,
            self.offering_end,
            self.scheduled_process_time,
        ]
        .windows(2)
        .all(|pair| pair[0] <= pair[1])
        {
            return Err(WwdStorageCorruptionError::GenesisWwdPhaseOrderInvalid.into());
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
            status: status_tag(day.status),
            day_type: day.day_type.as_u8(),
            forming_start: day.forming_start,
            forming_end: day.forming_end,
            lookback_end: day.lookback_end,
            offering_end: day.offering_end,
            scheduled_process_time: day.scheduled_process_time,
            metadosis_limit_minor: day.metadosis_limit_minor,
            previous_vwap_minor: day.previous_vwap_minor,
            current_vwap_minor: day.current_vwap_minor,
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
        if WwdStatus::try_from(day.status)? != WwdStatus::Offering {
            return Err(WwdStorageCorruptionError::GenesisWwdNotInOffering.into());
        }
        if day.forming_end > offering_end || day.lookback_end > offering_end {
            return Err(WwdStorageCorruptionError::GenesisWwdPhasesEndAfterOfferingDeadline.into());
        }
        let operation_changed =
            day.offering_end != offering_end || day.scheduled_process_time != offering_end;
        day.offering_end = offering_end;
        day.scheduled_process_time = offering_end;
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
        let Some(day) = contract.worldwide_days.get(expected_worldwide_day)? else {
            if active.is_empty() {
                return Ok(None);
            }
            return Err(WwdStorageCorruptionError::GenesisActiveWwdRecordMissing.into());
        };
        if WwdStatus::try_from(day.status)? != WwdStatus::Offering {
            return Err(WwdStorageCorruptionError::GenesisSeededWwdNotInOffering.into());
        }
        if !active.is_empty() {
            contract.active_wwd.remove(&expected_worldwide_day)?;
        }
        contract.worldwide_days.delete(expected_worldwide_day)?;
        Ok(Some(true))
    }
}

const fn status_tag(value: WwdStatus) -> u8 {
    match value {
        WwdStatus::Forming => status::FORMING,
        WwdStatus::LookbackDelay => status::LOOKBACK_DELAY,
        WwdStatus::Offering => status::OFFERING,
        WwdStatus::Waiting => status::WAITING,
        WwdStatus::Ready => status::READY,
        WwdStatus::Completed => status::COMPLETED,
        WwdStatus::Failed => status::FAILED,
    }
}

#[cfg(test)]
mod tests {
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    use super::*;
    use crate::worldwideday::{aggregate::WwdMembership, api};

    const DAY: WorldwideDay = WorldwideDay::new(20_260_731);

    fn offering_day() -> GenesisWorldwideDay {
        GenesisWorldwideDay {
            worldwide_day: DAY,
            status: WwdStatus::Offering,
            day_type: WwdDayType::Green,
            forming_start: 10,
            forming_end: 20,
            lookback_end: 30,
            offering_end: 40,
            scheduled_process_time: 40,
            metadosis_limit_minor: U256::from(100),
            previous_vwap_minor: U256::from(8),
            current_vwap_minor: U256::from(9),
        }
    }

    #[test]
    fn seed_retime_and_clear_preserve_the_typed_aggregate() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(1);

        StorageHandle::enter(&mut provider, |storage| {
            let report = FreshDevnetGenesisBuilder::new()
                .seed_active_worldwide_day(offering_day())
                .retime_offering_day(DAY, 35)
                .apply(storage.clone())
                .unwrap();
            assert!(report.changed);

            let projection = api::worldwide_day(storage.clone(), DAY).unwrap().unwrap();
            assert_eq!(projection.status, WwdStatus::Offering);
            assert_eq!(projection.membership, WwdMembership::Active);
            assert_eq!(projection.offering_end, 35);
            assert_eq!(projection.scheduled_process_time, 35);

            let report = FreshDevnetGenesisBuilder::new()
                .clear_single_offering_day(DAY)
                .apply(storage.clone())
                .unwrap();
            assert!(report.changed);
            assert!(api::worldwide_days(storage).unwrap().is_empty());
        });
    }

    #[test]
    fn rejects_post_launch_and_invalid_predecessors() {
        let mut post_launch = HashMapStorageProvider::new(1);
        post_launch.set_block_number(2);
        let error = StorageHandle::enter(&mut post_launch, |storage| {
            FreshDevnetGenesisBuilder::new()
                .seed_active_worldwide_day(offering_day())
                .apply(storage)
                .unwrap_err()
        });
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Revert(_)
        ));
        assert!(post_launch.storage.is_empty());

        let mut invalid = offering_day();
        invalid.status = WwdStatus::Completed;
        let mut provider = HashMapStorageProvider::new(1);
        let error = StorageHandle::enter(&mut provider, |storage| {
            FreshDevnetGenesisBuilder::new()
                .seed_active_worldwide_day(invalid)
                .apply(storage)
                .unwrap_err()
        });
        assert!(matches!(
            error,
            outbe_primitives::error::PrecompileError::Revert(_)
        ));
        assert!(provider.storage.is_empty());
    }

    #[test]
    fn clear_refuses_an_unexpected_active_day() {
        let unexpected = WorldwideDay::new(20_260_730);
        let mut day = offering_day();
        day.worldwide_day = unexpected;
        let mut provider = HashMapStorageProvider::new(1);

        StorageHandle::enter(&mut provider, |storage| {
            FreshDevnetGenesisBuilder::new()
                .seed_active_worldwide_day(day)
                .apply(storage.clone())
                .unwrap();
            let error = FreshDevnetGenesisBuilder::new()
                .clear_single_offering_day(DAY)
                .apply(storage.clone())
                .unwrap_err();
            assert!(matches!(
                error,
                outbe_primitives::error::PrecompileError::Revert(_)
            ));
            assert_eq!(api::worldwide_days(storage).unwrap().len(), 1);
        });
    }
}
