//! Bounded raw-storage slot plans for historical OCOMP Oracle openings.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::storage::types::StorageKey;
use outbe_primitives::time::WorldwideDay;

use crate::errors::OracleOcompError;

const PAIR_INDEX_BASE_SLOT: u64 = 10;
const SCURVE_COUNT_SLOT: u64 = 34;
const SCURVE_PAIR_BASE_SLOT: u64 = 35;
const SCURVE_PEAK_DAY_BASE_SLOT: u64 = 36;
const SCURVE_PEAK_PRICE_BASE_SLOT: u64 = 37;
const SCURVE_OLDEST_SLOT: u64 = 38;
const WWD_VWAP_EXISTS_BASE_SLOT: u64 = 47;
// Slots 50 and 51 held the day's pair count and its entry-ordinal -> pair
// column. Both are retired: the value column below is keyed by the registry
// index, so `pair_to_index` names the entry and nothing has to be enumerated.
const WWD_VWAP_VALUE_BASE_SLOT: u64 = 52;
const REFERENCE_CURRENCIES_SLOT: u64 = 55;

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
    /// Parallel to `reference_isos`: the registry index of each subject pair,
    /// `0` when that pair is not registered.
    pub pair_indices: Vec<u32>,
    pub scurve_count: u32,
    pub scurve_oldest: u32,
    pub slots: Vec<B256>,
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

/// Round one: the probe whose *values* size and address round two.
///
/// Two kinds of word live here. The four leading counters bound the collections
/// round two walks. After them comes one `pair_to_index` word per reference ISO.
/// The day-VWAP column is keyed by the registry index, so round two cannot
/// address a pair's value slot until it reads that index. The pair hash for an
/// ISO is derived rather than stored, so these slots need no on-chain count to
/// enumerate. That is exactly why round one can open them this early.
pub fn oracle_count_slot_plan_v1(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
) -> Result<OracleCountSlotPlanV1, OracleOcompError> {
    validate_reference_isos(reference_isos)?;
    let mut slots = vec![
        direct_slot(REFERENCE_CURRENCIES_SLOT),
        mapping_slot(worldwide_day, WWD_VWAP_EXISTS_BASE_SLOT),
        direct_slot(SCURVE_COUNT_SLOT),
        direct_slot(SCURVE_OLDEST_SLOT),
    ];
    slots.extend(pair_index_slots(reference_isos));
    Ok(OracleCountSlotPlanV1 {
        worldwide_day,
        reference_isos: reference_isos.to_vec(),
        slots,
    })
}

/// The number of leading counter words in a round-one plan, before the
/// per-ISO `pair_to_index` words start.
pub const ORACLE_COUNT_SLOTS_V1: usize = 4;

/// One `pair_to_index` slot per reference ISO, in ISO order.
///
/// `validate_reference_isos` has already rejected duplicates, and distinct ISOs
/// give distinct pairs, so these are distinct slots.
fn pair_index_slots(reference_isos: &[u16]) -> impl Iterator<Item = B256> + '_ {
    reference_isos
        .iter()
        .map(|iso| mapping_slot(AddressPair::new_coen_to(*iso), PAIR_INDEX_BASE_SLOT))
}

/// Rebuilds an S-curve entry's pair from the two words its
/// `Mapping<_, AddressPair>` value occupies.
fn checked_pair(base_word: U256, quote_word: U256) -> AddressPair {
    AddressPair::from_addresses(
        Address::from_word(base_word.into()),
        Address::from_word(quote_word.into()),
    )
}

/// Round-one values that size and address round two.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleOpeningCountsV1 {
    /// Length of the on-chain reference-currency list.
    pub reference_currency_count: u32,
    /// Parallel to the reference ISOs: the registry index of each subject pair,
    /// `0` when that pair is not registered.
    pub pair_indices: Vec<u32>,
    pub scurve_count: u32,
    pub scurve_oldest: u32,
}

impl OracleOpeningCountsV1 {
    /// Checks the counts against `reference_isos` and the caps. Returns the
    /// number of active S-curve entries.
    fn active_scurve_count(&self, reference_isos: &[u16]) -> Result<u32, OracleOcompError> {
        if self.pair_indices.len() != reference_isos.len() {
            return Err(OracleOcompError::PairIndexCountMismatch {
                actual: self.pair_indices.len(),
                expected: reference_isos.len(),
            });
        }
        if self.reference_currency_count > MAX_OCOMP_REFERENCE_CURRENCIES {
            return Err(OracleOcompError::ReferenceCurrencyCountExceedsCap {
                actual: self.reference_currency_count,
                cap: MAX_OCOMP_REFERENCE_CURRENCIES,
            });
        }
        let active_scurve_count = self.scurve_count.checked_sub(self.scurve_oldest).ok_or(
            OracleOcompError::ScurveOldestExceedsCount {
                oldest: self.scurve_oldest,
                count: self.scurve_count,
            },
        )?;
        if active_scurve_count > MAX_OCOMP_ACTIVE_SCURVE_ENTRIES {
            return Err(OracleOcompError::ActiveScurveCountExceedsCap {
                actual: active_scurve_count,
                cap: MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
            });
        }
        Ok(active_scurve_count)
    }
}

/// Round two: the full plan, addressed by the values round one returned.
///
/// `counts.pair_indices` is parallel to `reference_isos`. Entry `i` is the
/// registry index `pair_to_index` holds for `COEN/reference_isos[i]`, as read
/// from the round-one opening. A zero index means the pair is unregistered. That
/// pair has no value slot to open, and `evaluate_oracle_opening_v1` rejects it.
pub fn oracle_opening_slot_plan_v1(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
    counts: &OracleOpeningCountsV1,
) -> Result<OracleOpeningSlotPlanV1, OracleOcompError> {
    validate_reference_isos(reference_isos)?;
    let active_scurve_count = counts.active_scurve_count(reference_isos)?;
    let slots = opening_slots(worldwide_day, reference_isos, counts, active_scurve_count)?;

    Ok(OracleOpeningSlotPlanV1 {
        worldwide_day,
        reference_isos: reference_isos.to_vec(),
        reference_currency_count: counts.reference_currency_count,
        pair_indices: counts.pair_indices.clone(),
        scurve_count: counts.scurve_count,
        scurve_oldest: counts.scurve_oldest,
        slots,
    })
}

/// The round-two slots in canonical order: the reference-currency list, the
/// pair-index words, the WorldwideDay VWAP words, and the active S-curve words.
fn opening_slots(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
    counts: &OracleOpeningCountsV1,
    active_scurve_count: u32,
) -> Result<Vec<B256>, OracleOcompError> {
    let scurve_slots = usize::from(u16::try_from(active_scurve_count).map_err(|_| {
        OracleOcompError::ActiveScurveCountExceedsCap {
            actual: active_scurve_count,
            cap: MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
        }
    })?)
    .saturating_mul(4);
    let mut slots = Vec::with_capacity(
        reference_isos
            .len()
            .saturating_mul(2)
            .saturating_add(counts.reference_currency_count as usize)
            .saturating_add(4)
            .saturating_add(scurve_slots),
    );
    // The on-chain reference-currency list, so the verifier can prove every
    // subject ISO is actually registered.
    slots.push(direct_slot(REFERENCE_CURRENCIES_SLOT));
    for index in 0..counts.reference_currency_count {
        slots.push(vec_element_slot(REFERENCE_CURRENCIES_SLOT, index));
    }
    let mut seen = BTreeSet::new();
    for pair_index_slot in pair_index_slots(reference_isos) {
        if seen.insert(pair_index_slot) {
            slots.push(pair_index_slot);
        }
    }
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_EXISTS_BASE_SLOT));
    // One value word per registered subject pair, addressed by its registry
    // index. Unregistered pairs (index 0) have nothing to open.
    for index in counts
        .pair_indices
        .iter()
        .copied()
        .filter(|index| *index != 0)
    {
        slots.push(nested_mapping_slot(
            worldwide_day,
            WWD_VWAP_VALUE_BASE_SLOT,
            index,
        ));
    }
    slots.push(direct_slot(SCURVE_COUNT_SLOT));
    slots.push(direct_slot(SCURVE_OLDEST_SLOT));
    for index in counts.scurve_oldest..counts.scurve_count {
        let pair_slot = mapping_slot(index, SCURVE_PAIR_BASE_SLOT);
        slots.push(pair_slot);
        slots.push(next_slot(pair_slot));
        slots.push(mapping_slot(index, SCURVE_PEAK_DAY_BASE_SLOT));
        slots.push(mapping_slot(index, SCURVE_PEAK_PRICE_BASE_SLOT));
    }
    Ok(slots)
}

/// Opened slot values, keyed by slot.
struct SlotValues(BTreeMap<B256, U256>);

impl SlotValues {
    /// Indexes `ordered_slots`. Rejects a slot that occurs twice.
    fn new(ordered_slots: &[(B256, U256)]) -> Result<Self, OracleOcompError> {
        let mut values = BTreeMap::new();
        for (slot, value) in ordered_slots {
            if values.insert(*slot, *value).is_some() {
                return Err(OracleOcompError::DuplicateSlot(*slot));
            }
        }
        Ok(Self(values))
    }

    fn at(&self, slot: B256) -> Result<U256, OracleOcompError> {
        self.0
            .get(&slot)
            .copied()
            .ok_or(OracleOcompError::MissingSlot(slot))
    }
}

pub fn evaluate_oracle_opening_v1(
    worldwide_day: WorldwideDay,
    reference_isos: &[u16],
    ordered_slots: &[(B256, U256)],
) -> Result<OracleOpeningEvaluationV1, OracleOcompError> {
    let count_plan = oracle_count_slot_plan_v1(worldwide_day, reference_isos)?;
    let values = SlotValues::new(ordered_slots)?;
    let (counts, worldwide_day_exists) = read_round_one(&count_plan, &values)?;
    let plan = oracle_opening_slot_plan_v1(worldwide_day, reference_isos, &counts)?;
    if ordered_slots
        .iter()
        .map(|(slot, _)| *slot)
        .ne(plan.slots.iter().copied())
    {
        return Err(OracleOcompError::NonCanonicalSlotSequence);
    }

    check_scurve_words(&counts, &values)?;
    require_reference_currencies(reference_isos, counts.reference_currency_count, &values)?;

    let mut ordered_entry_prices = Vec::with_capacity(reference_isos.len());
    for (iso, index) in reference_isos.iter().copied().zip(counts.pair_indices) {
        // The proven index is both the registration witness and the key: a zero
        // means the verifier is pricing an unregistered pair.
        if index == 0 {
            return Err(OracleOcompError::PairNotRegistered { iso });
        }
        // `pair_to_index` is keyed by the sorted pair, so a market registered as
        // `<iso>/COEN` resolves to the same index and prices identically.
        // Direction-insensitivity now comes from the key, not from a scan.
        let vwap = if worldwide_day_exists.is_zero() {
            U256::ZERO
        } else {
            values.at(nested_mapping_slot(
                worldwide_day,
                WWD_VWAP_VALUE_BASE_SLOT,
                index,
            ))?
        };
        ordered_entry_prices.push((iso, vwap));
    }
    Ok(OracleOpeningEvaluationV1 {
        ordered_entry_prices,
    })
}

/// Reads the round-one counters and pair indices. Returns them with the
/// WorldwideDay `exists` word, which must be 0 or 1.
fn read_round_one(
    count_plan: &OracleCountSlotPlanV1,
    values: &SlotValues,
) -> Result<(OracleOpeningCountsV1, U256), OracleOcompError> {
    let reference_currency_count =
        checked_u32(values.at(count_plan.slots[0])?, "reference currency count")?;
    let worldwide_day_exists = values.at(count_plan.slots[1])?;
    if worldwide_day_exists > U256::from(1) {
        return Err(OracleOcompError::InvalidWorldwideDayExists(
            worldwide_day_exists,
        ));
    }
    let scurve_count = checked_u32(values.at(count_plan.slots[2])?, "S-curve count")?;
    let scurve_oldest = checked_u32(values.at(count_plan.slots[3])?, "S-curve oldest index")?;
    let mut pair_indices = Vec::with_capacity(count_plan.reference_isos.len());
    for slot in count_plan.slots.iter().skip(ORACLE_COUNT_SLOTS_V1) {
        pair_indices.push(checked_u32(values.at(*slot)?, "reference pair index")?);
    }
    let counts = OracleOpeningCountsV1 {
        reference_currency_count,
        pair_indices,
        scurve_count,
        scurve_oldest,
    };
    Ok((counts, worldwide_day_exists))
}

/// Checks that every active S-curve entry opens to a well-formed pair, peak
/// day and peak price.
fn check_scurve_words(
    counts: &OracleOpeningCountsV1,
    values: &SlotValues,
) -> Result<(), OracleOcompError> {
    for index in counts.scurve_oldest..counts.scurve_count {
        let pair_slot = mapping_slot(index, SCURVE_PAIR_BASE_SLOT);
        let _pair = checked_pair(values.at(pair_slot)?, values.at(next_slot(pair_slot))?);
        checked_u64(
            values.at(mapping_slot(index, SCURVE_PEAK_DAY_BASE_SLOT))?,
            "S-curve peak day",
        )?;
        let _peak_price = values.at(mapping_slot(index, SCURVE_PEAK_PRICE_BASE_SLOT))?;
    }
    Ok(())
}

/// Every subject ISO must be a registered on-chain reference currency.
fn require_reference_currencies(
    reference_isos: &[u16],
    reference_currency_count: u32,
    values: &SlotValues,
) -> Result<(), OracleOcompError> {
    let mut registered = BTreeSet::new();
    for index in 0..reference_currency_count {
        let word = values.at(vec_element_slot(REFERENCE_CURRENCIES_SLOT, index))?;
        registered.insert(checked_u16(word, "reference currency ISO")?);
    }
    if let Some(iso) = reference_isos.iter().find(|iso| !registered.contains(iso)) {
        return Err(OracleOcompError::IsoNotAReferenceCurrency { iso: *iso });
    }
    Ok(())
}

fn checked_u32(value: U256, field: &'static str) -> Result<u32, OracleOcompError> {
    if value > U256::from(u32::MAX) {
        Err(OracleOcompError::IntegerOverflow(field))
    } else {
        Ok(value.to::<u32>())
    }
}

fn checked_u16(value: U256, field: &'static str) -> Result<u16, OracleOcompError> {
    if value > U256::from(u16::MAX) {
        Err(OracleOcompError::IntegerOverflow(field))
    } else {
        Ok(value.to::<u16>())
    }
}

fn checked_u64(value: U256, field: &'static str) -> Result<u64, OracleOcompError> {
    if value > U256::from(u64::MAX) {
        Err(OracleOcompError::IntegerOverflow(field))
    } else {
        Ok(value.to::<u64>())
    }
}

fn validate_reference_isos(isos: &[u16]) -> Result<(), OracleOcompError> {
    if isos.len() > MAX_OCOMP_REFERENCE_ISOS {
        return Err(OracleOcompError::ReferenceIsoCountExceedsCap {
            actual: isos.len(),
            cap: MAX_OCOMP_REFERENCE_ISOS,
        });
    }
    if !isos.contains(&MANDATORY_USD_ISO) {
        return Err(OracleOcompError::MissingMandatoryUsdReference);
    }
    if isos.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(OracleOcompError::NonCanonicalReferenceIsos);
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
