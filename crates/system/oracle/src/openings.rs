//! Bounded raw-storage slot plans for historical OCOMP Oracle openings.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, Formatter},
};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::storage::types::StorageKey;

use crate::types::coen_iso_pair;

const PAIR_INDEX_BASE_SLOT: u64 = 10;
const SCURVE_COUNT_SLOT: u64 = 34;
const SCURVE_PAIR_BASE_SLOT: u64 = 35;
const SCURVE_PEAK_DAY_BASE_SLOT: u64 = 36;
const SCURVE_PEAK_PRICE_BASE_SLOT: u64 = 37;
const SCURVE_OLDEST_SLOT: u64 = 38;
const WWD_VWAP_EXISTS_BASE_SLOT: u64 = 47;
const WWD_VWAP_PAIR_COUNT_BASE_SLOT: u64 = 50;
const WWD_VWAP_PAIR_BASE_SLOT: u64 = 51;
const WWD_VWAP_VALUE_BASE_SLOT: u64 = 52;
const REFERENCE_CURRENCIES_SLOT: u64 = 55;

pub const MAX_OCOMP_WWD_PAIR_ENTRIES: u32 = 256;
pub const MAX_OCOMP_ACTIVE_SCURVE_ENTRIES: u32 = 256;
pub const MAX_OCOMP_REFERENCE_ISOS: usize = 256;
pub const MAX_OCOMP_REFERENCE_CURRENCIES: u32 = 256;
const MANDATORY_USD_ISO: u16 = 840;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleCountSlotPlanV1 {
    pub worldwide_day: WorldwideDay,
    pub reference_isos: Vec<u16>,
    pub slots: Vec<B256>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleOpeningSlotPlanV1 {
    pub worldwide_day: WorldwideDay,
    pub reference_isos: Vec<u16>,
    pub reference_currency_count: u32,
    pub worldwide_day_pair_count: u32,
    pub scurve_count: u32,
    pub scurve_oldest: u32,
    pub slots: Vec<B256>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OracleOpeningPlanError {
    MissingMandatoryUsdReference,
    NonCanonicalReferenceIsos,
    ReferenceIsoCountExceedsCap { actual: usize, cap: usize },
    ReferenceCurrencyCountExceedsCap { actual: u32, cap: u32 },
    WorldwideDayPairCountExceedsCap { actual: u32, cap: u32 },
    ScurveOldestExceedsCount { oldest: u32, count: u32 },
    ActiveScurveCountExceedsCap { actual: u32, cap: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleOpeningEvaluationV1 {
    pub ordered_entry_prices: Vec<(u16, U256)>,
}

impl OracleOpeningEvaluationV1 {
    #[must_use]
    pub fn entry_price(&self, iso: u16) -> Option<U256> {
        self.ordered_entry_prices
            .binary_search_by_key(&iso, |(candidate, _)| *candidate)
            .ok()
            .map(|index| self.ordered_entry_prices[index].1)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OracleOpeningEvaluationError {
    Plan(OracleOpeningPlanError),
    DuplicateSlot(B256),
    MissingSlot(B256),
    NonCanonicalSlotSequence,
    IntegerOverflow(&'static str),
    InvalidWorldwideDayExists(U256),
    PairNotRegistered { iso: u16 },
    IsoNotAReferenceCurrency { iso: u16 },
}

impl Display for OracleOpeningEvaluationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan(error) => write!(formatter, "{error}"),
            Self::DuplicateSlot(slot) => write!(formatter, "duplicate Oracle opening slot {slot}"),
            Self::MissingSlot(slot) => write!(formatter, "missing Oracle opening slot {slot}"),
            Self::NonCanonicalSlotSequence => {
                formatter.write_str("Oracle opening slots do not match the canonical plan")
            }
            Self::IntegerOverflow(field) => {
                write!(
                    formatter,
                    "Oracle opening {field} does not fit its runtime type"
                )
            }
            Self::InvalidWorldwideDayExists(value) => {
                write!(formatter, "invalid Oracle WorldwideDay exists flag {value}")
            }
            Self::PairNotRegistered { iso } => {
                write!(formatter, "Oracle ISO {iso} names an unregistered pair")
            }
            Self::IsoNotAReferenceCurrency { iso } => {
                write!(
                    formatter,
                    "Oracle ISO {iso} is not an on-chain reference currency"
                )
            }
        }
    }
}

impl std::error::Error for OracleOpeningEvaluationError {}

impl From<OracleOpeningPlanError> for OracleOpeningEvaluationError {
    fn from(error: OracleOpeningPlanError) -> Self {
        Self::Plan(error)
    }
}

impl Display for OracleOpeningPlanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMandatoryUsdReference => {
                formatter.write_str("reference ISO list does not contain mandatory 840")
            }
            Self::NonCanonicalReferenceIsos => {
                formatter.write_str("reference ISO list is not strictly ascending")
            }
            Self::ReferenceIsoCountExceedsCap { actual, cap } => {
                write!(formatter, "reference ISO count {actual} exceeds cap {cap}")
            }
            Self::ReferenceCurrencyCountExceedsCap { actual, cap } => {
                write!(
                    formatter,
                    "on-chain reference currency count {actual} exceeds cap {cap}"
                )
            }
            Self::WorldwideDayPairCountExceedsCap { actual, cap } => {
                write!(formatter, "WWD VWAP pair count {actual} exceeds cap {cap}")
            }
            Self::ScurveOldestExceedsCount { oldest, count } => {
                write!(
                    formatter,
                    "S-curve oldest index {oldest} exceeds count {count}"
                )
            }
            Self::ActiveScurveCountExceedsCap { actual, cap } => {
                write!(formatter, "active S-curve count {actual} exceeds cap {cap}")
            }
        }
    }
}

impl std::error::Error for OracleOpeningPlanError {}

/// Round one: the fixed-size count probe.
///
/// The full plan's length depends on collection sizes that are themselves on
/// chain, so this opens only the five counter words. It is independent of the
/// ISO list — the pair hash for an ISO is derived, not stored.
pub fn oracle_count_slot_plan_v1(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
) -> Result<OracleCountSlotPlanV1, OracleOpeningPlanError> {
    validate_reference_isos(reference_isos)?;
    let slots = vec![
        direct_slot(REFERENCE_CURRENCIES_SLOT),
        mapping_slot(worldwide_day, WWD_VWAP_EXISTS_BASE_SLOT),
        mapping_slot(worldwide_day, WWD_VWAP_PAIR_COUNT_BASE_SLOT),
        direct_slot(SCURVE_COUNT_SLOT),
        direct_slot(SCURVE_OLDEST_SLOT),
    ];
    Ok(OracleCountSlotPlanV1 {
        worldwide_day,
        reference_isos: reference_isos.to_vec(),
        slots,
    })
}

/// Rebuilds an entry's pair from the two words its `Mapping<_, AddressPair>`
/// value occupies.
fn checked_pair(base_word: U256, quote_word: U256) -> AddressPair {
    AddressPair::quoted(
        Address::from_word(base_word.into()),
        Address::from_word(quote_word.into()),
    )
}

pub fn oracle_opening_slot_plan_v1(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
    reference_currency_count: u32,
    worldwide_day_pair_count: u32,
    scurve_count: u32,
    scurve_oldest: u32,
) -> Result<OracleOpeningSlotPlanV1, OracleOpeningPlanError> {
    validate_reference_isos(reference_isos)?;
    if reference_currency_count > MAX_OCOMP_REFERENCE_CURRENCIES {
        return Err(OracleOpeningPlanError::ReferenceCurrencyCountExceedsCap {
            actual: reference_currency_count,
            cap: MAX_OCOMP_REFERENCE_CURRENCIES,
        });
    }
    if worldwide_day_pair_count > MAX_OCOMP_WWD_PAIR_ENTRIES {
        return Err(OracleOpeningPlanError::WorldwideDayPairCountExceedsCap {
            actual: worldwide_day_pair_count,
            cap: MAX_OCOMP_WWD_PAIR_ENTRIES,
        });
    }
    let active_scurve_count = scurve_count.checked_sub(scurve_oldest).ok_or(
        OracleOpeningPlanError::ScurveOldestExceedsCount {
            oldest: scurve_oldest,
            count: scurve_count,
        },
    )?;
    if active_scurve_count > MAX_OCOMP_ACTIVE_SCURVE_ENTRIES {
        return Err(OracleOpeningPlanError::ActiveScurveCountExceedsCap {
            actual: active_scurve_count,
            cap: MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
        });
    }

    let wwd_slots = usize::from(u16::try_from(worldwide_day_pair_count).map_err(|_| {
        OracleOpeningPlanError::WorldwideDayPairCountExceedsCap {
            actual: worldwide_day_pair_count,
            cap: MAX_OCOMP_WWD_PAIR_ENTRIES,
        }
    })?)
    .saturating_mul(3);
    let scurve_slots = usize::from(u16::try_from(active_scurve_count).map_err(|_| {
        OracleOpeningPlanError::ActiveScurveCountExceedsCap {
            actual: active_scurve_count,
            cap: MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
        }
    })?)
    .saturating_mul(4);
    let mut slots = Vec::with_capacity(
        reference_isos
            .len()
            .saturating_add(reference_currency_count as usize)
            .saturating_add(5)
            .saturating_add(wwd_slots)
            .saturating_add(scurve_slots),
    );
    // The on-chain reference-currency list, so the verifier can prove every
    // subject ISO is actually registered.
    slots.push(direct_slot(REFERENCE_CURRENCIES_SLOT));
    for index in 0..reference_currency_count {
        slots.push(vec_element_slot(REFERENCE_CURRENCIES_SLOT, index));
    }
    let mut pair_index_slots = BTreeSet::new();
    for iso in reference_isos {
        let pair_index_slot = mapping_slot(coen_iso_pair(*iso), PAIR_INDEX_BASE_SLOT);
        if pair_index_slots.insert(pair_index_slot) {
            slots.push(pair_index_slot);
        }
    }
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_EXISTS_BASE_SLOT));
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_PAIR_COUNT_BASE_SLOT));
    for index in 0..worldwide_day_pair_count {
        let pair_slot = nested_mapping_slot(worldwide_day, WWD_VWAP_PAIR_BASE_SLOT, index);
        slots.push(pair_slot);
        slots.push(next_slot(pair_slot));
        slots.push(nested_mapping_slot(
            worldwide_day,
            WWD_VWAP_VALUE_BASE_SLOT,
            index,
        ));
    }
    slots.push(direct_slot(SCURVE_COUNT_SLOT));
    slots.push(direct_slot(SCURVE_OLDEST_SLOT));
    for index in scurve_oldest..scurve_count {
        let pair_slot = mapping_slot(index, SCURVE_PAIR_BASE_SLOT);
        slots.push(pair_slot);
        slots.push(next_slot(pair_slot));
        slots.push(mapping_slot(index, SCURVE_PEAK_DAY_BASE_SLOT));
        slots.push(mapping_slot(index, SCURVE_PEAK_PRICE_BASE_SLOT));
    }

    Ok(OracleOpeningSlotPlanV1 {
        worldwide_day,
        reference_isos: reference_isos.to_vec(),
        reference_currency_count,
        worldwide_day_pair_count,
        scurve_count,
        scurve_oldest,
        slots,
    })
}

pub fn evaluate_oracle_opening_v1(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
    ordered_slots: &[(B256, U256)],
) -> Result<OracleOpeningEvaluationV1, OracleOpeningEvaluationError> {
    let count_plan = oracle_count_slot_plan_v1(worldwide_day, reference_isos)?;
    let mut values = BTreeMap::new();
    for (slot, value) in ordered_slots {
        if values.insert(*slot, *value).is_some() {
            return Err(OracleOpeningEvaluationError::DuplicateSlot(*slot));
        }
    }
    let value_at = |slot: B256| {
        values
            .get(&slot)
            .copied()
            .ok_or(OracleOpeningEvaluationError::MissingSlot(slot))
    };
    let reference_currency_count =
        checked_u32(value_at(count_plan.slots[0])?, "reference currency count")?;
    let worldwide_day_exists = value_at(count_plan.slots[1])?;
    if worldwide_day_exists > U256::from(1) {
        return Err(OracleOpeningEvaluationError::InvalidWorldwideDayExists(
            worldwide_day_exists,
        ));
    }
    let worldwide_day_pair_count =
        checked_u32(value_at(count_plan.slots[2])?, "WorldwideDay pair count")?;
    let scurve_count = checked_u32(value_at(count_plan.slots[3])?, "S-curve count")?;
    let scurve_oldest = checked_u32(value_at(count_plan.slots[4])?, "S-curve oldest index")?;
    let plan = oracle_opening_slot_plan_v1(
        worldwide_day,
        reference_isos,
        reference_currency_count,
        worldwide_day_pair_count,
        scurve_count,
        scurve_oldest,
    )?;
    if ordered_slots
        .iter()
        .map(|(slot, _)| *slot)
        .ne(plan.slots.iter().copied())
    {
        return Err(OracleOpeningEvaluationError::NonCanonicalSlotSequence);
    }

    let mut worldwide_day_vwaps = Vec::with_capacity(worldwide_day_pair_count as usize);
    for index in 0..worldwide_day_pair_count {
        let pair_slot = nested_mapping_slot(worldwide_day, WWD_VWAP_PAIR_BASE_SLOT, index);
        worldwide_day_vwaps.push((
            checked_pair(value_at(pair_slot)?, value_at(next_slot(pair_slot))?),
            value_at(nested_mapping_slot(
                worldwide_day,
                WWD_VWAP_VALUE_BASE_SLOT,
                index,
            ))?,
        ));
    }
    let mut scurves = Vec::with_capacity((scurve_count - scurve_oldest) as usize);
    for index in scurve_oldest..scurve_count {
        let pair_slot = mapping_slot(index, SCURVE_PAIR_BASE_SLOT);
        scurves.push((
            checked_pair(value_at(pair_slot)?, value_at(next_slot(pair_slot))?),
            checked_u64(
                value_at(mapping_slot(index, SCURVE_PEAK_DAY_BASE_SLOT))?,
                "S-curve peak day",
            )?,
            value_at(mapping_slot(index, SCURVE_PEAK_PRICE_BASE_SLOT))?,
        ));
    }

    // Every subject ISO must be a registered on-chain reference currency.
    let mut registered = BTreeSet::new();
    for index in 0..reference_currency_count {
        let word = value_at(vec_element_slot(REFERENCE_CURRENCIES_SLOT, index))?;
        registered.insert(checked_u16(word, "reference currency ISO")?);
    }
    if let Some(iso) = reference_isos.iter().find(|iso| !registered.contains(iso)) {
        return Err(OracleOpeningEvaluationError::IsoNotAReferenceCurrency { iso: *iso });
    }

    let target_day = crate::scurve::truncate_to_day(worldwide_day.to_timestamp_utc());
    let mut ordered_entry_prices = Vec::with_capacity(reference_isos.len());
    for iso in reference_isos.iter().copied() {
        let pair = coen_iso_pair(iso);
        // The index is still proven, purely as the registration witness: a zero
        // here means the verifier is pricing an unregistered pair.
        let index = checked_u32(
            value_at(mapping_slot(pair, PAIR_INDEX_BASE_SLOT))?,
            "reference pair index",
        )?;
        if index == 0 {
            return Err(OracleOpeningEvaluationError::PairNotRegistered { iso });
        }
        // Entries are matched on the market, not the quote direction: a pair
        // registered as `<iso>/COEN` still prices, exactly as it did when both
        // sides of this comparison were sorted keys.
        let vwap = if worldwide_day_exists.is_zero() {
            U256::ZERO
        } else {
            worldwide_day_vwaps
                .iter()
                .find_map(|(candidate, value)| candidate.same_market(&pair).then_some(*value))
                .unwrap_or(U256::ZERO)
        };
        let mut max_scurve = U256::ZERO;
        for (entry_pair, peak_day, peak_price) in &scurves {
            if !entry_pair.same_market(&pair) || target_day < *peak_day {
                continue;
            }
            let days_since = ((target_day - *peak_day) / crate::scurve::DAY_SECONDS) as usize;
            if days_since >= crate::scurve::PERIOD {
                continue;
            }
            max_scurve =
                max_scurve.max(crate::scurve::compute_scurve_value(*peak_price, days_since));
        }
        ordered_entry_prices.push((iso, vwap.max(max_scurve)));
    }
    Ok(OracleOpeningEvaluationV1 {
        ordered_entry_prices,
    })
}

fn checked_u32(value: U256, field: &'static str) -> Result<u32, OracleOpeningEvaluationError> {
    if value > U256::from(u32::MAX) {
        Err(OracleOpeningEvaluationError::IntegerOverflow(field))
    } else {
        Ok(value.to::<u32>())
    }
}

fn checked_u16(value: U256, field: &'static str) -> Result<u16, OracleOpeningEvaluationError> {
    if value > U256::from(u16::MAX) {
        Err(OracleOpeningEvaluationError::IntegerOverflow(field))
    } else {
        Ok(value.to::<u16>())
    }
}

fn checked_u64(value: U256, field: &'static str) -> Result<u64, OracleOpeningEvaluationError> {
    if value > U256::from(u64::MAX) {
        Err(OracleOpeningEvaluationError::IntegerOverflow(field))
    } else {
        Ok(value.to::<u64>())
    }
}

fn validate_reference_isos(isos: &[u16]) -> Result<(), OracleOpeningPlanError> {
    if isos.len() > MAX_OCOMP_REFERENCE_ISOS {
        return Err(OracleOpeningPlanError::ReferenceIsoCountExceedsCap {
            actual: isos.len(),
            cap: MAX_OCOMP_REFERENCE_ISOS,
        });
    }
    if !isos.contains(&MANDATORY_USD_ISO) {
        return Err(OracleOpeningPlanError::MissingMandatoryUsdReference);
    }
    if isos.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(OracleOpeningPlanError::NonCanonicalReferenceIsos);
    }
    Ok(())
}

/// `StorageVec` element address: `keccak256(be32(base_slot)) + index`. Every
/// `Storable` in this repo occupies one whole word, so the stride is 1.
fn vec_element_slot(base_slot: u64, index: u32) -> B256 {
    let start = U256::from_be_bytes(keccak256(U256::from(base_slot).to_be_bytes::<32>()).0);
    raw_slot(start + U256::from(index))
}

fn nested_mapping_slot(
    outer_key: impl StorageKey,
    base_slot: u64,
    inner_key: impl StorageKey,
) -> B256 {
    let inner_base = outer_key.mapping_slot(U256::from(base_slot));
    raw_slot(inner_key.mapping_slot(inner_base))
}

fn mapping_slot(key: impl StorageKey, base_slot: u64) -> B256 {
    raw_slot(key.mapping_slot(U256::from(base_slot)))
}

fn direct_slot(slot: u64) -> B256 {
    raw_slot(U256::from(slot))
}

/// The second word of a two-word `AddressPair` mapping value: the quote sits
/// immediately after the base, in the key's own hashed namespace.
fn next_slot(slot: B256) -> B256 {
    raw_slot(U256::from_be_bytes(slot.0) + U256::ONE)
}

fn raw_slot(slot: U256) -> B256 {
    B256::new(slot.to_be_bytes())
}
