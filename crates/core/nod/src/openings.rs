//! Exact storage proofs for the Oracle UTC-day VWAPs that price certified Lysis.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    address_pair::AddressPair,
    error::Result,
    storage::types::StorageKey,
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::errors::NodError;

pub const MAX_ENTRY_PRICE_CURRENCIES: u32 = 256;
const PAIR_TO_INDEX_SLOT: u64 = 10;
const UTC_DAY_VWAP_SLOT: u64 = 58;

/// The UTC day priced by a request: the last one closed before its block.
#[must_use]
pub fn entry_price_day(logical_evaluation_time: u64) -> u32 {
    previous_date_key(timestamp_to_date_key(logical_evaluation_time))
}

/// One `pair_to_index` word per subject ISO. The ISOs are ordered, bounded, and include USD.
pub fn pair_index_slots(isos: &[u16]) -> Result<Vec<B256>> {
    if isos.len() > MAX_ENTRY_PRICE_CURRENCIES as usize
        || isos.first() == Some(&0)
        || !isos.contains(&840)
        || isos.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(NodError::InvalidEntryPriceOpening.into());
    }
    let base = U256::from(PAIR_TO_INDEX_SLOT);
    Ok(isos
        .iter()
        .map(|iso| word(AddressPair::new_coen_to(*iso).mapping_slot(base)))
        .collect())
}

/// The pair-index words, then the day's VWAP of every registered pair in ISO order.
pub fn entry_price_slots(day: u32, isos: &[u16], pair_indices: &[u32]) -> Result<Vec<B256>> {
    let mut slots = pair_index_slots(isos)?;
    if pair_indices.len() != isos.len() {
        return Err(NodError::InvalidEntryPriceOpening.into());
    }
    let day_base = day.mapping_slot(U256::from(UTC_DAY_VWAP_SLOT));
    slots.extend(
        pair_indices
            .iter()
            .filter(|index| **index != 0)
            .map(|index| word(index.mapping_slot(day_base))),
    );
    Ok(slots)
}

/// The caller authenticates these slots against the request's state root.
/// An unregistered pair or a day without data leaves its currency unpriced.
pub fn evaluate_entry_prices(
    day: u32,
    isos: &[u16],
    ordered_slots: &[(B256, U256)],
) -> Result<BTreeMap<u16, U256>> {
    let pair_indices = ordered_slots
        .iter()
        .take(isos.len())
        .map(|(_, value)| {
            u32::try_from(*value).map_err(|_| NodError::InvalidEntryPriceOpening.into())
        })
        .collect::<Result<Vec<_>>>()?;
    let slots = entry_price_slots(day, isos, &pair_indices)?;
    if ordered_slots.len() != slots.len()
        || ordered_slots.iter().map(|(slot, _)| slot).ne(slots.iter())
    {
        return Err(NodError::InvalidEntryPriceOpening.into());
    }
    let mut vwaps = ordered_slots[isos.len()..].iter().map(|(_, vwap)| *vwap);
    Ok(isos
        .iter()
        .zip(&pair_indices)
        .filter(|(_, index)| **index != 0)
        .filter_map(|(iso, _)| vwaps.next().map(|vwap| (*iso, vwap)))
        .filter(|(_, vwap)| !vwap.is_zero())
        .collect())
}

fn word(slot: U256) -> B256 {
    B256::from(slot.to_be_bytes())
}
