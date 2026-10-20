use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::schema::TributeContract;

use super::{
    constants::{MAX_ACTIVE_WWDS, MAX_RECORDS_KEPT, MAX_RETAINED_WWDS},
    errors::WwdStorageCorruptionError,
    schema::{day_type, status, WorldwideDay as WorldwideDayRecord},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WwdStatus {
    Forming = status::FORMING,
    LookbackDelay = status::LOOKBACK_DELAY,
    Offering = status::OFFERING,
    Waiting = status::WAITING,
    Ready = status::READY,
    Completed = status::COMPLETED,
    Failed = status::FAILED,
}

impl WwdStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for WwdStatus {
    type Error = PrecompileError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            status::FORMING => Ok(Self::Forming),
            status::LOOKBACK_DELAY => Ok(Self::LookbackDelay),
            status::OFFERING => Ok(Self::Offering),
            status::WAITING => Ok(Self::Waiting),
            status::READY => Ok(Self::Ready),
            status::COMPLETED => Ok(Self::Completed),
            status::FAILED => Ok(Self::Failed),
            other => Err(WwdStorageCorruptionError::WwdUnknownStatusTag { other }.into()),
        }
    }
}

impl PartialEq<u8> for WwdStatus {
    fn eq(&self, other: &u8) -> bool {
        self.as_u8() == *other
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WwdDayType {
    Unknown = day_type::UNKNOWN,
    Green = day_type::GREEN,
    Red = day_type::RED,
}

impl TryFrom<u8> for WwdDayType {
    type Error = PrecompileError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            day_type::UNKNOWN => Ok(Self::Unknown),
            day_type::GREEN => Ok(Self::Green),
            day_type::RED => Ok(Self::Red),
            other => Err(WwdStorageCorruptionError::WwdUnknownDayTypeTag { other }.into()),
        }
    }
}

impl PartialEq<u8> for WwdDayType {
    fn eq(&self, other: &u8) -> bool {
        self.as_u8() == *other
    }
}

impl WwdDayType {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WwdMembership {
    Active,
    Closed,
}

/// Immutable, typed read projection for one persisted WorldwideDay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WwdProjection {
    pub worldwide_day: WorldwideDay,
    pub status: WwdStatus,
    pub day_type: WwdDayType,
    pub membership: WwdMembership,
    pub forming_start: u64,
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled_process_time: u64,
    pub metadosis_limit_minor: U256,
    pub previous_vwap_minor: U256,
    pub current_vwap_minor: U256,
}

impl WwdProjection {
    /// Maps one already-decoded storage record into the shared typed shape.
    /// Callers retain their distinct membership and validation contracts.
    pub(crate) fn from_record(
        worldwide_day: WorldwideDay,
        status: WwdStatus,
        day_type: WwdDayType,
        membership: WwdMembership,
        record: &WorldwideDayRecord,
    ) -> Self {
        Self {
            worldwide_day,
            status,
            day_type,
            membership,
            forming_start: record.forming_start,
            forming_end: record.forming_end,
            lookback_end: record.lookback_end,
            offering_end: record.offering_end,
            scheduled_process_time: record.scheduled_process_time,
            metadosis_limit_minor: record.metadosis_limit_minor,
            previous_vwap_minor: record.previous_vwap_minor,
            current_vwap_minor: record.current_vwap_minor,
        }
    }
}

/// Complete bounded snapshot of indexed WWD state validated before any
/// production command effect.
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

    pub fn ready_records(&self) -> impl Iterator<Item = &WwdProjection> {
        self.active_records()
            .filter(|record| record.status == WwdStatus::Ready)
    }

    pub fn retained_count(&self) -> usize {
        self.active_records()
            .filter(|record| matches!(record.status, WwdStatus::Ready))
            .count()
    }

    pub fn validate_capacity_victim(&self, victim: &WwdProjection) -> Result<()> {
        let victim_key = (victim.scheduled_process_time, victim.worldwide_day);
        if self.active_records().any(|record| {
            matches!(record.status, WwdStatus::Ready)
                && (record.scheduled_process_time, record.worldwide_day) > victim_key
        }) {
            return Err(
                WwdStorageCorruptionError::CapacityForfeitureCandidateOlderThanRetainedWork.into(),
            );
        }
        Ok(())
    }

    pub fn ensure_can_insert_active(&self, wwd: WorldwideDay) -> Result<()> {
        if self.records.contains_key(&wwd) {
            return Err(
                WwdStorageCorruptionError::WwdAlreadyExistsBeforeActiveInsertion { wwd }.into(),
            );
        }
        if self.active_order.len() >= MAX_ACTIVE_WWDS {
            return Err(WwdStorageCorruptionError::ActiveWwdCapReached {
                max_active_wwds: MAX_ACTIVE_WWDS,
                wwd,
            }
            .into());
        }
        Ok(())
    }

    pub fn load_and_validate(storage: StorageHandle<'_>) -> Result<Self> {
        let contract = TributeContract::new(storage);
        let (active_set, closed_set) = load_index_membership(&contract)?;
        let records = load_records(&contract, &active_set, &closed_set)?;
        let active_order = build_active_order(active_set, &records);
        let aggregate = Self {
            records,
            active_order,
        };
        let retained_count = aggregate.retained_count();
        if retained_count > MAX_RETAINED_WWDS {
            return Err(WwdStorageCorruptionError::RetainedWwdPopulationExceedsCap {
                retained_count,
                max_retained_wwds: MAX_RETAINED_WWDS,
            }
            .into());
        }
        Ok(aggregate)
    }
}

fn load_index_membership(
    contract: &TributeContract<'_>,
) -> Result<(BTreeSet<WorldwideDay>, BTreeSet<WorldwideDay>)> {
    let active_set = collect_unique("active", contract.active_wwd.read_all()?)?;
    let closed_set = collect_unique("closed", contract.closed_wwd.read_all()?)?;
    if active_set.len() > MAX_ACTIVE_WWDS {
        return Err(
            WwdStorageCorruptionError::ActiveWwdPopulationExceedsDerivedCap {
                count: active_set.len(),
                max_active_wwds: MAX_ACTIVE_WWDS,
            }
            .into(),
        );
    }
    if closed_set.len() > MAX_RECORDS_KEPT {
        return Err(
            WwdStorageCorruptionError::ClosedWwdPopulationExceedsRetentionCap {
                count: closed_set.len(),
                max_records_kept: MAX_RECORDS_KEPT,
            }
            .into(),
        );
    }
    if let Some(overlap) = active_set.intersection(&closed_set).next() {
        return Err(WwdStorageCorruptionError::WwdBothActiveAndClosed { overlap: *overlap }.into());
    }
    Ok((active_set, closed_set))
}

fn load_records(
    contract: &TributeContract<'_>,
    active_set: &BTreeSet<WorldwideDay>,
    closed_set: &BTreeSet<WorldwideDay>,
) -> Result<BTreeMap<WorldwideDay, WwdProjection>> {
    let mut records = BTreeMap::new();
    for (membership, indexed) in [
        (WwdMembership::Active, active_set),
        (WwdMembership::Closed, closed_set),
    ] {
        for wwd in indexed {
            let record = contract.worldwide_days.get(*wwd)?.ok_or(
                WwdStorageCorruptionError::IndexPointsToMissingWwd {
                    membership,
                    wwd: *wwd,
                },
            )?;
            let status = WwdStatus::try_from(record.status)?;
            let day_type = WwdDayType::try_from(record.day_type)?;
            validate_record_shape(*wwd, status, membership, &record)?;
            records.insert(
                *wwd,
                WwdProjection::from_record(*wwd, status, day_type, membership, &record),
            );
        }
    }
    Ok(records)
}

fn validate_record_shape(
    wwd: WorldwideDay,
    status: WwdStatus,
    membership: WwdMembership,
    record: &WorldwideDayRecord,
) -> Result<()> {
    if status.is_terminal() != matches!(membership, WwdMembership::Closed) {
        return Err(WwdStorageCorruptionError::WwdStatusMembershipMismatch { wwd }.into());
    }
    if ![
        record.forming_start,
        record.forming_end,
        record.lookback_end,
        record.offering_end,
        record.scheduled_process_time,
    ]
    .windows(2)
    .all(|pair| pair[0] <= pair[1])
    {
        return Err(WwdStorageCorruptionError::WwdNonMonotonicPhaseBoundaries { wwd }.into());
    }
    Ok(())
}

fn build_active_order(
    active_set: BTreeSet<WorldwideDay>,
    records: &BTreeMap<WorldwideDay, WwdProjection>,
) -> Vec<WorldwideDay> {
    let mut active_order = active_set.into_iter().collect::<Vec<_>>();
    active_order.sort_by_key(|wwd| {
        let record = records.get(wwd).expect("validated active record");
        (record.scheduled_process_time, record.worldwide_day)
    });
    active_order
}

fn collect_unique(
    label: &'static str,
    values: Vec<WorldwideDay>,
) -> Result<BTreeSet<WorldwideDay>> {
    let mut set = BTreeSet::new();
    for value in values {
        if !set.insert(value) {
            return Err(
                WwdStorageCorruptionError::WwdIndexContainsDuplicate { label, value }.into(),
            );
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    use super::*;

    fn wwd() -> WorldwideDay {
        WorldwideDay::new(20260730)
    }

    fn record(status: u8, day_type: u8) -> WorldwideDayRecord {
        WorldwideDayRecord {
            wwd: wwd(),
            status,
            day_type,
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            scheduled_process_time: 5,
            metadosis_limit_minor: U256::ZERO,
            previous_vwap_minor: U256::ZERO,
            current_vwap_minor: U256::ZERO,
        }
    }

    fn seed_active(provider: &mut HashMapStorageProvider, record: &WorldwideDayRecord) {
        provider
            .enter(|storage| {
                let contract = TributeContract::new(storage);
                contract.worldwide_days.create(record)?;
                contract.active_wwd.insert(record.wwd)?;
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn unknown_persisted_status_is_rejected_for_green_and_red_before_effects() {
        for day_type in [day_type::GREEN, day_type::RED] {
            let mut provider = HashMapStorageProvider::new(1);
            seed_active(&mut provider, &record(5, day_type));
            provider
                .enter(|storage| {
                    assert!(matches!(
                        ValidatedWwdAggregate::load_and_validate(storage),
                        Err(outbe_primitives::error::PrecompileError::Revert(_))
                    ));
                    Ok::<_, outbe_primitives::error::PrecompileError>(())
                })
                .unwrap();
        }
    }

    #[test]
    fn typed_tags_reject_unrecognized_values() {
        assert!(WwdStatus::try_from(255).is_err());
        assert!(WwdDayType::try_from(255).is_err());
    }

    #[test]
    fn aggregate_rejects_active_closed_overlap() {
        let mut provider = HashMapStorageProvider::new(1);
        let terminal = record(status::COMPLETED, day_type::GREEN);
        provider
            .enter(|storage| {
                let contract = TributeContract::new(storage.clone());
                contract.worldwide_days.create(&terminal)?;
                contract.active_wwd.insert(terminal.wwd)?;
                contract.closed_wwd.push_back(terminal.wwd)?;
                let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
                assert!(error.to_string().contains("both active and closed"));
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();
    }

    #[test]
    fn aggregate_rejects_missing_record_status_membership_and_phase_drift() {
        let mut missing = HashMapStorageProvider::new(1);
        missing
            .enter(|storage| {
                let contract = TributeContract::new(storage.clone());
                contract.active_wwd.insert(wwd())?;
                let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
                assert!(error.to_string().contains("missing WWD"));
                Ok::<_, outbe_primitives::error::PrecompileError>(())
            })
            .unwrap();

        let mut membership = HashMapStorageProvider::new(1);
        seed_active(&mut membership, &record(status::COMPLETED, day_type::GREEN));
        membership.enter(|storage| {
            let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
            assert!(error.to_string().contains("status/membership mismatch"));
        });

        let mut boundaries = HashMapStorageProvider::new(1);
        let mut invalid = record(status::FORMING, day_type::UNKNOWN);
        invalid.forming_end = 0;
        seed_active(&mut boundaries, &invalid);
        boundaries.enter(|storage| {
            let error = ValidatedWwdAggregate::load_and_validate(storage).unwrap_err();
            assert!(error.to_string().contains("non-monotonic"));
        });
    }
}
