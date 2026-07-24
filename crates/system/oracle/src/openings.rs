//! Bounded raw-storage slot plans for historical OCOMP Oracle openings.

use std::{
    collections::BTreeSet,
    fmt::{Display, Formatter},
};

use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::storage::types::StorageKey;

const PAIR_HASH_TO_ID_BASE_SLOT: u64 = 10;
const SCURVE_COUNT_SLOT: u64 = 34;
const SCURVE_PAIR_ID_BASE_SLOT: u64 = 35;
const SCURVE_PEAK_DAY_BASE_SLOT: u64 = 36;
const SCURVE_PEAK_PRICE_BASE_SLOT: u64 = 37;
const SCURVE_OLDEST_SLOT: u64 = 38;
const SETTLEMENT_ISO_TO_DENOM_BASE_SLOT: u64 = 41;
const SETTLEMENT_ISO_TO_PAIR_BASE_SLOT: u64 = 42;
const WWD_VWAP_EXISTS_BASE_SLOT: u64 = 47;
const WWD_VWAP_PAIR_COUNT_BASE_SLOT: u64 = 50;
const WWD_VWAP_PAIR_ID_BASE_SLOT: u64 = 51;
const WWD_VWAP_VALUE_BASE_SLOT: u64 = 52;

pub const MAX_OCOMP_WWD_PAIR_ENTRIES: u32 = 256;
pub const MAX_OCOMP_ACTIVE_SCURVE_ENTRIES: u32 = 256;
pub const MAX_OCOMP_SETTLEMENT_CURRENCIES: usize = 256;
const MANDATORY_USD_ISO: u16 = 840;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleCountSlotPlanV1 {
    pub worldwide_day: WorldwideDay,
    pub settlement_isos: Vec<u16>,
    pub slots: Vec<B256>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleOpeningSlotPlanV1 {
    pub worldwide_day: WorldwideDay,
    pub settlement_pairs: Vec<(u16, B256)>,
    pub worldwide_day_pair_count: u32,
    pub scurve_count: u32,
    pub scurve_oldest: u32,
    pub slots: Vec<B256>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OracleOpeningPlanError {
    MissingMandatoryUsdSettlement,
    NonCanonicalSettlementIsos,
    SettlementCurrencyCountExceedsCap { actual: usize, cap: usize },
    WorldwideDayPairCountExceedsCap { actual: u32, cap: u32 },
    ScurveOldestExceedsCount { oldest: u32, count: u32 },
    ActiveScurveCountExceedsCap { actual: u32, cap: u32 },
}

impl Display for OracleOpeningPlanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMandatoryUsdSettlement => {
                formatter.write_str("settlement ISO list does not contain mandatory 840")
            }
            Self::NonCanonicalSettlementIsos => {
                formatter.write_str("settlement ISO list is not strictly ascending")
            }
            Self::SettlementCurrencyCountExceedsCap { actual, cap } => {
                write!(
                    formatter,
                    "settlement currency count {actual} exceeds cap {cap}"
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

pub fn oracle_count_slot_plan_v1(
    worldwide_day: WorldwideDay,
    settlement_isos: &[u16],
) -> Result<OracleCountSlotPlanV1, OracleOpeningPlanError> {
    validate_settlement_isos(settlement_isos)?;
    let mut slots = Vec::with_capacity(settlement_isos.len().saturating_mul(2) + 4);
    for iso in settlement_isos {
        slots.push(mapping_slot(*iso, SETTLEMENT_ISO_TO_DENOM_BASE_SLOT));
        slots.push(mapping_slot(*iso, SETTLEMENT_ISO_TO_PAIR_BASE_SLOT));
    }
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_EXISTS_BASE_SLOT));
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_PAIR_COUNT_BASE_SLOT));
    slots.push(direct_slot(SCURVE_COUNT_SLOT));
    slots.push(direct_slot(SCURVE_OLDEST_SLOT));
    Ok(OracleCountSlotPlanV1 {
        worldwide_day,
        settlement_isos: settlement_isos.to_vec(),
        slots,
    })
}

pub fn oracle_opening_slot_plan_v1(
    worldwide_day: WorldwideDay,
    settlement_pairs: &[(u16, B256)],
    worldwide_day_pair_count: u32,
    scurve_count: u32,
    scurve_oldest: u32,
) -> Result<OracleOpeningSlotPlanV1, OracleOpeningPlanError> {
    let settlement_isos = settlement_pairs
        .iter()
        .map(|(iso, _)| *iso)
        .collect::<Vec<_>>();
    validate_settlement_isos(&settlement_isos)?;
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
    .saturating_mul(2);
    let scurve_slots = usize::from(u16::try_from(active_scurve_count).map_err(|_| {
        OracleOpeningPlanError::ActiveScurveCountExceedsCap {
            actual: active_scurve_count,
            cap: MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
        }
    })?)
    .saturating_mul(3);
    let mut slots = Vec::with_capacity(
        settlement_pairs
            .len()
            .saturating_mul(3)
            .saturating_add(4)
            .saturating_add(wwd_slots)
            .saturating_add(scurve_slots),
    );
    let mut pair_id_slots = BTreeSet::new();
    for (iso, pair_hash) in settlement_pairs {
        slots.push(mapping_slot(*iso, SETTLEMENT_ISO_TO_DENOM_BASE_SLOT));
        slots.push(mapping_slot(*iso, SETTLEMENT_ISO_TO_PAIR_BASE_SLOT));
        let pair_id_slot = mapping_slot(*pair_hash, PAIR_HASH_TO_ID_BASE_SLOT);
        if pair_id_slots.insert(pair_id_slot) {
            slots.push(pair_id_slot);
        }
    }
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_EXISTS_BASE_SLOT));
    slots.push(mapping_slot(worldwide_day, WWD_VWAP_PAIR_COUNT_BASE_SLOT));
    for index in 0..worldwide_day_pair_count {
        slots.push(nested_mapping_slot(
            worldwide_day,
            WWD_VWAP_PAIR_ID_BASE_SLOT,
            index,
        ));
        slots.push(nested_mapping_slot(
            worldwide_day,
            WWD_VWAP_VALUE_BASE_SLOT,
            index,
        ));
    }
    slots.push(direct_slot(SCURVE_COUNT_SLOT));
    slots.push(direct_slot(SCURVE_OLDEST_SLOT));
    for index in scurve_oldest..scurve_count {
        slots.push(mapping_slot(index, SCURVE_PAIR_ID_BASE_SLOT));
        slots.push(mapping_slot(index, SCURVE_PEAK_DAY_BASE_SLOT));
        slots.push(mapping_slot(index, SCURVE_PEAK_PRICE_BASE_SLOT));
    }

    Ok(OracleOpeningSlotPlanV1 {
        worldwide_day,
        settlement_pairs: settlement_pairs.to_vec(),
        worldwide_day_pair_count,
        scurve_count,
        scurve_oldest,
        slots,
    })
}

fn validate_settlement_isos(isos: &[u16]) -> Result<(), OracleOpeningPlanError> {
    if isos.len() > MAX_OCOMP_SETTLEMENT_CURRENCIES {
        return Err(OracleOpeningPlanError::SettlementCurrencyCountExceedsCap {
            actual: isos.len(),
            cap: MAX_OCOMP_SETTLEMENT_CURRENCIES,
        });
    }
    if !isos.contains(&MANDATORY_USD_ISO) {
        return Err(OracleOpeningPlanError::MissingMandatoryUsdSettlement);
    }
    if isos.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(OracleOpeningPlanError::NonCanonicalSettlementIsos);
    }
    Ok(())
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

fn raw_slot(slot: U256) -> B256 {
    B256::new(slot.to_be_bytes())
}
