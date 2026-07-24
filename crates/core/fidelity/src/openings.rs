//! Bounded raw-storage slot plans for historical OCOMP Fidelity openings.

use std::fmt::{Display, Formatter};

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::storage::types::StorageKey;

use crate::schema::{active_cohort_key, sold_cohort_key};

const QUALIFIED_START_BASE_SLOT: u64 = 0;
const ACTIVE_COUNT_BASE_SLOT: u64 = 1;
const ACTIVE_COHORTS_BASE_SLOT: u64 = 2;
const SOLD_COUNT_BASE_SLOT: u64 = 4;
const SOLD_COHORTS_BASE_SLOT: u64 = 5;
const FIRST_QUALIFIED_START_SLOT: u64 = 8;

/// Frozen PoC cap checked before allocating cohort-detail slot vectors.
pub const MAX_OCOMP_COHORTS_PER_OWNER: u32 = 64;

/// Small first-phase slot plan used to authenticate counts before detail
/// allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FidelityCountSlotPlanV1 {
    pub owner: Address,
    pub qualified_start: B256,
    pub first_qualified_start: B256,
    pub active_count: B256,
    pub sold_count: B256,
}

impl FidelityCountSlotPlanV1 {
    #[must_use]
    pub const fn slots(self) -> [B256; 4] {
        [
            self.qualified_start,
            self.first_qualified_start,
            self.active_count,
            self.sold_count,
        ]
    }
}

/// Complete, canonically ordered raw slot plan for one owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FidelityOpeningSlotPlanV1 {
    pub owner: Address,
    pub active_count: u32,
    pub sold_count: u32,
    pub slots: Vec<B256>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FidelityOpeningPlanError {
    ActiveCountExceedsCap { actual: u32, cap: u32 },
    SoldCountExceedsCap { actual: u32, cap: u32 },
}

impl Display for FidelityOpeningPlanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveCountExceedsCap { actual, cap } => {
                write!(formatter, "active cohort count {actual} exceeds cap {cap}")
            }
            Self::SoldCountExceedsCap { actual, cap } => {
                write!(formatter, "sold cohort count {actual} exceeds cap {cap}")
            }
        }
    }
}

impl std::error::Error for FidelityOpeningPlanError {}

#[must_use]
pub fn fidelity_count_slot_plan_v1(owner: Address) -> FidelityCountSlotPlanV1 {
    FidelityCountSlotPlanV1 {
        owner,
        qualified_start: mapping_slot(owner, QUALIFIED_START_BASE_SLOT),
        first_qualified_start: direct_slot(FIRST_QUALIFIED_START_SLOT),
        active_count: mapping_slot(owner, ACTIVE_COUNT_BASE_SLOT),
        sold_count: mapping_slot(owner, SOLD_COUNT_BASE_SLOT),
    }
}

pub fn fidelity_opening_slot_plan_v1(
    owner: Address,
    active_count: u32,
    sold_count: u32,
) -> Result<FidelityOpeningSlotPlanV1, FidelityOpeningPlanError> {
    if active_count > MAX_OCOMP_COHORTS_PER_OWNER {
        return Err(FidelityOpeningPlanError::ActiveCountExceedsCap {
            actual: active_count,
            cap: MAX_OCOMP_COHORTS_PER_OWNER,
        });
    }
    if sold_count > MAX_OCOMP_COHORTS_PER_OWNER {
        return Err(FidelityOpeningPlanError::SoldCountExceedsCap {
            actual: sold_count,
            cap: MAX_OCOMP_COHORTS_PER_OWNER,
        });
    }

    let counts = fidelity_count_slot_plan_v1(owner);
    let mut slots = Vec::new();
    slots.push(counts.qualified_start);
    slots.push(counts.first_qualified_start);
    slots.push(counts.active_count);
    for index in 0..active_count {
        let key = active_cohort_key(owner, index);
        slots.push(mapping_slot(key, ACTIVE_COHORTS_BASE_SLOT));
        slots.push(mapping_slot(key, ACTIVE_COHORTS_BASE_SLOT + 1));
    }
    slots.push(counts.sold_count);
    for index in 0..sold_count {
        let key = sold_cohort_key(owner, index);
        slots.push(mapping_slot(key, SOLD_COHORTS_BASE_SLOT));
        slots.push(mapping_slot(key, SOLD_COHORTS_BASE_SLOT + 1));
        slots.push(mapping_slot(key, SOLD_COHORTS_BASE_SLOT + 2));
    }
    Ok(FidelityOpeningSlotPlanV1 {
        owner,
        active_count,
        sold_count,
        slots,
    })
}

fn mapping_slot(key: impl StorageKey, base_slot: u64) -> B256 {
    raw_slot(key.mapping_slot(U256::from(base_slot)))
}

fn direct_slot(slot: u64) -> B256 {
    raw_slot(U256::from(slot))
}

fn raw_slot(slot: U256) -> B256 {
    B256::new(slot.to_be_bytes())
}
