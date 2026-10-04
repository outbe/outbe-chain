mod ocomp_indexes;
use ocomp_indexes::validate_ocomp_index_equivalence;

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::{
    constants::{MAX_ACTIVE_WWDS, MAX_RECORDS_KEPT, MAX_RETAINED_WWDS},
    errors::storage_corruption_message,
    schema::{day_type, status, MetadosisContract},
    terminal::{model::WwdTerminalReceipt, TerminalReceiptValidationContext},
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
    OffchainPending = status::OFFCHAIN_PENDING,
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
            status::OFFCHAIN_PENDING => Ok(Self::OffchainPending),
            other => Err(storage_corruption_message(format!(
                "Metadosis WWD has unknown status tag {other}"
            ))),
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
            other => Err(storage_corruption_message(format!(
                "Metadosis WWD has unknown day-type tag {other}"
            ))),
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
    pub metadosis_limit_amount: U256,
    pub previous_vwap: U256,
    pub current_vwap: U256,
}

impl WwdProjection {
    /// Maps one already-decoded storage record into the shared typed shape.
    /// Callers retain their distinct membership and validation contracts.
    pub(crate) fn from_record(
        worldwide_day: WorldwideDay,
        status: WwdStatus,
        day_type: WwdDayType,
        membership: WwdMembership,
        record: &crate::schema::WorldwideDay,
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
            metadosis_limit_amount: record.metadosis_limit_amount,
            previous_vwap: record.previous_vwap,
            current_vwap: record.current_vwap,
        }
    }
}

/// Complete bounded snapshot of indexed WWD state validated before any
/// production command effect.
#[derive(Debug)]
pub(crate) struct ValidatedWwdAggregate {
    records: BTreeMap<WorldwideDay, WwdProjection>,
    active_order: Vec<WorldwideDay>,
}

impl ValidatedWwdAggregate {
    pub(crate) fn record(&self, worldwide_day: WorldwideDay) -> Option<&WwdProjection> {
        self.records.get(&worldwide_day)
    }

    pub(crate) fn active_records(&self) -> impl Iterator<Item = &WwdProjection> {
        self.active_order
            .iter()
            .map(|wwd| self.records.get(wwd).expect("validated active order"))
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = &WwdProjection> {
        self.records.values()
    }

    pub(crate) fn ready_records(&self) -> impl Iterator<Item = &WwdProjection> {
        self.active_records()
            .filter(|record| record.status == WwdStatus::Ready)
    }

    pub(crate) fn retained_count(&self) -> usize {
        self.active_records()
            .filter(|record| matches!(record.status, WwdStatus::Ready | WwdStatus::OffchainPending))
            .count()
    }

    pub(crate) fn validate_capacity_victim(&self, victim: &WwdProjection) -> Result<()> {
        let victim_key = (victim.scheduled_process_time, victim.worldwide_day);
        if self.active_records().any(|record| {
            matches!(record.status, WwdStatus::Ready | WwdStatus::OffchainPending)
                && (record.scheduled_process_time, record.worldwide_day) > victim_key
        }) {
            return Err(storage_corruption_message(
                "CapacityForfeiture candidate is older than retained OCOMP work",
            ));
        }
        Ok(())
    }

    pub(crate) fn ensure_can_insert_active(&self, wwd: WorldwideDay) -> Result<()> {
        if self.records.contains_key(&wwd) {
            return Err(storage_corruption_message(format!(
                "Metadosis WWD {wwd} already exists before active insertion"
            )));
        }
        if self.active_order.len() >= MAX_ACTIVE_WWDS {
            return Err(storage_corruption_message(format!(
                "Metadosis active WWD cap {MAX_ACTIVE_WWDS} reached before inserting {wwd}"
            )));
        }
        Ok(())
    }

    pub(crate) fn load_and_validate(storage: StorageHandle<'_>) -> Result<Self> {
        let contract = MetadosisContract::new(storage);
        let (active_set, closed_set) = load_index_membership(&contract)?;
        let records = load_records(&contract, &active_set, &closed_set)?;
        validate_ocomp_index_equivalence(&contract, &records, &active_set)?;
        let active_order = build_active_order(active_set, &records);
        let aggregate = Self {
            records,
            active_order,
        };
        let retained_count = aggregate.retained_count();
        if retained_count > MAX_RETAINED_WWDS {
            return Err(storage_corruption_message(format!(
                "Metadosis retained WWD population {retained_count} exceeds OCOMP cap {MAX_RETAINED_WWDS}"
            )));
        }
        Ok(aggregate)
    }
}

fn load_index_membership(
    contract: &MetadosisContract<'_>,
) -> Result<(BTreeSet<WorldwideDay>, BTreeSet<WorldwideDay>)> {
    let active_set = collect_unique("active", contract.active_wwd.read_all()?)?;
    let closed_set = collect_unique("closed", contract.closed_wwd.read_all()?)?;
    if active_set.len() > MAX_ACTIVE_WWDS {
        return Err(storage_corruption_message(format!(
            "Metadosis active WWD population {} exceeds derived cap {MAX_ACTIVE_WWDS}",
            active_set.len()
        )));
    }
    if closed_set.len() > MAX_RECORDS_KEPT {
        return Err(storage_corruption_message(format!(
            "Metadosis closed WWD population {} exceeds retention cap {MAX_RECORDS_KEPT}",
            closed_set.len()
        )));
    }
    if let Some(overlap) = active_set.intersection(&closed_set).next() {
        return Err(storage_corruption_message(format!(
            "Metadosis WWD {overlap} is both active and closed"
        )));
    }
    Ok((active_set, closed_set))
}

fn load_records(
    contract: &MetadosisContract<'_>,
    active_set: &BTreeSet<WorldwideDay>,
    closed_set: &BTreeSet<WorldwideDay>,
) -> Result<BTreeMap<WorldwideDay, WwdProjection>> {
    let mut records = BTreeMap::new();
    for (membership, indexed) in [
        (WwdMembership::Active, active_set),
        (WwdMembership::Closed, closed_set),
    ] {
        for wwd in indexed {
            let record = contract.worldwide_days.get(*wwd)?.ok_or_else(|| {
                storage_corruption_message(format!(
                    "Metadosis {membership:?} index points to missing WWD {wwd}"
                ))
            })?;
            let status = WwdStatus::try_from(record.status)?;
            let day_type = WwdDayType::try_from(record.day_type)?;
            validate_record_shape(*wwd, status, membership, &record)?;
            validate_terminal_state(contract, *wwd, status, membership, &record)?;
            validate_record_ocomp_presence(contract, *wwd, status, membership)?;
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
    record: &crate::schema::WorldwideDay,
) -> Result<()> {
    if status.is_terminal() != matches!(membership, WwdMembership::Closed) {
        return Err(storage_corruption_message(format!(
            "Metadosis WWD {wwd} status/membership mismatch"
        )));
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
        return Err(storage_corruption_message(format!(
            "Metadosis WWD {wwd} has non-monotonic phase boundaries"
        )));
    }
    Ok(())
}

fn validate_terminal_state(
    contract: &MetadosisContract<'_>,
    wwd: WorldwideDay,
    status: WwdStatus,
    membership: WwdMembership,
    record: &crate::schema::WorldwideDay,
) -> Result<()> {
    let Some(receipt) = contract.read_terminal_receipt(wwd)? else {
        return Ok(());
    };
    let expected_value_routed = if matches!(receipt, WwdTerminalReceipt::MetadosisFailure(_)) {
        contract.failure_value_routed(wwd, record.metadosis_limit_amount)?
    } else {
        record.metadosis_limit_amount
    };
    receipt
        .validate(TerminalReceiptValidationContext::new(
            status.as_u8(),
            usize::from(membership == WwdMembership::Active),
            usize::from(membership == WwdMembership::Closed),
            expected_value_routed,
        ))
        .map_err(|_| {
            storage_corruption_message(format!(
                "Metadosis WWD {wwd} has an invalid terminal receipt"
            ))
        })
}

fn validate_record_ocomp_presence(
    contract: &MetadosisContract<'_>,
    wwd: WorldwideDay,
    status: WwdStatus,
    membership: WwdMembership,
) -> Result<()> {
    let has_ocomp_state = !contract.ocomp_fsm_states.get_bytes(&wwd).is_empty()?;
    if status == WwdStatus::OffchainPending && !has_ocomp_state {
        return Err(storage_corruption_message(format!(
            "Metadosis OFFCHAIN_PENDING WWD {wwd} has no OCOMP FSM"
        )));
    }
    if matches!(membership, WwdMembership::Closed) && has_ocomp_state {
        return Err(storage_corruption_message(format!(
            "Metadosis closed WWD {wwd} retains a live OCOMP FSM"
        )));
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

fn collect_unique(label: &str, values: Vec<WorldwideDay>) -> Result<BTreeSet<WorldwideDay>> {
    let mut set = BTreeSet::new();
    for value in values {
        if !set.insert(value) {
            return Err(storage_corruption_message(format!(
                "Metadosis {label} WWD index contains duplicate {value}"
            )));
        }
    }
    Ok(set)
}
