//! Bounded raw-storage slot plans for historical OCOMP Fidelity openings.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, Formatter},
};

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::storage::types::StorageKey;

use crate::math::{league_from_rcfi, t_dec, RcfiAccumulator};
use crate::schema::{active_cohort_key, sold_cohort_key};

const QUALIFIED_START_BASE_SLOT: u64 = 0;
const ACTIVE_COUNT_BASE_SLOT: u64 = 1;
const ACTIVE_COHORTS_BASE_SLOT: u64 = 2;
const SOLD_COUNT_BASE_SLOT: u64 = 4;
const SOLD_COHORTS_BASE_SLOT: u64 = 5;
const FIRST_QUALIFIED_START_SLOT: u64 = 8;

/// Frozen PoC cap checked before allocating cohort-detail slot vectors.
pub const MAX_OCOMP_COHORTS_PER_OWNER: u32 = 64;
pub const MAX_OCOMP_OWNERS_PER_OPENING: usize = 256;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FidelityLeagueObservationV1 {
    pub owner: Address,
    pub league: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FidelityOpeningEvaluationError {
    InvalidOwners,
    DuplicateSlot,
    MissingSlot,
    NonCanonicalSlotPlan,
    WordOutOfRange,
    ArithmeticOverflow,
    Plan(FidelityOpeningPlanError),
}

impl Display for FidelityOpeningEvaluationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOwners => formatter.write_str(
                "Fidelity opening owners must be non-empty, bounded, ordered and unique",
            ),
            Self::DuplicateSlot => formatter.write_str("Fidelity opening contains duplicate slots"),
            Self::MissingSlot => formatter.write_str("Fidelity opening is missing a required slot"),
            Self::NonCanonicalSlotPlan => {
                formatter.write_str("Fidelity opening does not match the exact slot plan")
            }
            Self::WordOutOfRange => {
                formatter.write_str("Fidelity opening word does not fit its field")
            }
            Self::ArithmeticOverflow => formatter.write_str("Fidelity opening evaluation overflow"),
            Self::Plan(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for FidelityOpeningEvaluationError {}

impl From<FidelityOpeningPlanError> for FidelityOpeningEvaluationError {
    fn from(error: FidelityOpeningPlanError) -> Self {
        Self::Plan(error)
    }
}

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

/// Evaluates the exact authenticated raw Fidelity slot plan without reading
/// contract state. The caller remains responsible for verifying the storage
/// proof against the finalized state root before passing these values.
pub fn evaluate_fidelity_opening_v1(
    owners: &[Address],
    ordered_slots: &[(B256, U256)],
    timestamp: u64,
) -> Result<Vec<FidelityLeagueObservationV1>, FidelityOpeningEvaluationError> {
    if owners.is_empty()
        || owners.len() > MAX_OCOMP_OWNERS_PER_OPENING
        || !owners.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err(FidelityOpeningEvaluationError::InvalidOwners);
    }

    let mut values = BTreeMap::new();
    for (slot, value) in ordered_slots {
        if values.insert(*slot, *value).is_some() {
            return Err(FidelityOpeningEvaluationError::DuplicateSlot);
        }
    }

    let mut expected_slots = Vec::new();
    let mut unique_slots = BTreeSet::new();
    let mut owner_plans = Vec::new();
    for owner in owners {
        let counts = fidelity_count_slot_plan_v1(*owner);
        let active_count = word_u32(required_word(&values, counts.active_count)?)?;
        let sold_count = word_u32(required_word(&values, counts.sold_count)?)?;
        let plan = fidelity_opening_slot_plan_v1(*owner, active_count, sold_count)?;
        for slot in &plan.slots {
            if unique_slots.insert(*slot) {
                expected_slots.push(*slot);
            }
        }
        owner_plans.push(plan);
    }
    if ordered_slots
        .iter()
        .map(|(slot, _)| *slot)
        .ne(expected_slots)
    {
        return Err(FidelityOpeningEvaluationError::NonCanonicalSlotPlan);
    }

    let first_qualified_start = word_u64(required_word(
        &values,
        fidelity_count_slot_plan_v1(owners[0]).first_qualified_start,
    )?)?;
    let maximum_rcfi = if first_qualified_start == 0 {
        U256::ZERO
    } else {
        t_dec(timestamp.saturating_sub(first_qualified_start))
    };

    let mut observations = Vec::new();
    observations
        .try_reserve_exact(owners.len())
        .map_err(|_| FidelityOpeningEvaluationError::ArithmeticOverflow)?;
    for (owner, plan) in owners.iter().zip(owner_plans) {
        let counts = fidelity_count_slot_plan_v1(*owner);
        let qualified_start = word_u64(required_word(&values, counts.qualified_start)?)?;
        let rcfi = evaluate_owner_rcfi(&values, &plan, qualified_start, timestamp)?;
        let league = league_from_rcfi(rcfi, maximum_rcfi)
            .ok_or(FidelityOpeningEvaluationError::ArithmeticOverflow)?;
        observations.push(FidelityLeagueObservationV1 {
            owner: *owner,
            league,
        });
    }
    Ok(observations)
}

fn evaluate_owner_rcfi(
    values: &BTreeMap<B256, U256>,
    plan: &FidelityOpeningSlotPlanV1,
    qualified_start: u64,
    timestamp: u64,
) -> Result<U256, FidelityOpeningEvaluationError> {
    if qualified_start == 0 {
        return Ok(U256::ZERO);
    }
    let mut cursor = 3_usize;
    let mut accumulator = RcfiAccumulator::default();
    for _ in 0..plan.active_count {
        let size = required_word(values, plan.slots[cursor])?;
        let acquired_at = word_u64(required_word(values, plan.slots[cursor + 1])?)?;
        cursor += 2;
        if size.is_zero() {
            continue;
        }
        accumulator
            .add_active(size, acquired_at, timestamp)
            .ok_or(FidelityOpeningEvaluationError::ArithmeticOverflow)?;
    }
    cursor += 1;
    for _ in 0..plan.sold_count {
        let size = required_word(values, plan.slots[cursor])?;
        let acquired_at = word_u64(required_word(values, plan.slots[cursor + 1])?)?;
        let sold_at = word_u64(required_word(values, plan.slots[cursor + 2])?)?;
        cursor += 3;
        if size.is_zero() {
            continue;
        }
        accumulator
            .add_sold(size, acquired_at, sold_at, timestamp)
            .ok_or(FidelityOpeningEvaluationError::ArithmeticOverflow)?;
    }
    accumulator
        .finish(qualified_start, timestamp)
        .map(|(rcfi, _, _)| rcfi)
        .ok_or(FidelityOpeningEvaluationError::ArithmeticOverflow)
}

fn required_word(
    values: &BTreeMap<B256, U256>,
    slot: B256,
) -> Result<U256, FidelityOpeningEvaluationError> {
    values
        .get(&slot)
        .copied()
        .ok_or(FidelityOpeningEvaluationError::MissingSlot)
}

fn word_u32(word: U256) -> Result<u32, FidelityOpeningEvaluationError> {
    if word > U256::from(u32::MAX) {
        Err(FidelityOpeningEvaluationError::WordOutOfRange)
    } else {
        Ok(word.to::<u32>())
    }
}

fn word_u64(word: U256) -> Result<u64, FidelityOpeningEvaluationError> {
    if word > U256::from(u64::MAX) {
        Err(FidelityOpeningEvaluationError::WordOutOfRange)
    } else {
        Ok(word.to::<u64>())
    }
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
