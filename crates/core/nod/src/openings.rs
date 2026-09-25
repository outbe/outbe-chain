//! Exact storage proofs for the entry-price snapshot used by certified Lysis.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_primitives::{error::Result, storage::types::StorageKey, time::WorldwideDay};

use crate::errors::NodError;

pub const MAX_ENTRY_PRICE_CURRENCIES: u32 = 256;
const FROZEN_SLOT: u64 = 30;
const PRICE_SLOT: u64 = 33;

/// The subject ISOs are ordered, bounded, and always include mandatory USD.
pub fn entry_price_slots(day: WorldwideDay, isos: &[u16]) -> Result<Vec<B256>> {
    if isos.len() > MAX_ENTRY_PRICE_CURRENCIES as usize
        || isos.first() == Some(&0)
        || !isos.contains(&840)
        || isos.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(NodError::InvalidEntryPriceSnapshot.into());
    }
    let price_base = day.mapping_slot(U256::from(PRICE_SLOT));
    let mut slots = Vec::with_capacity(isos.len() + 1);
    slots.push(B256::from(
        day.mapping_slot(U256::from(FROZEN_SLOT)).to_be_bytes(),
    ));
    slots.extend(
        isos.iter()
            .map(|iso| B256::from(iso.mapping_slot(price_base).to_be_bytes())),
    );
    Ok(slots)
}

/// The caller authenticates these slots against the job's frozen state root.
/// Unpriced currencies remain absent, just as in the runtime snapshot map.
pub fn evaluate_entry_prices(
    day: WorldwideDay,
    isos: &[u16],
    ordered_slots: &[(B256, U256)],
) -> Result<BTreeMap<u16, U256>> {
    let slots = entry_price_slots(day, isos)?;
    if ordered_slots.len() != slots.len()
        || ordered_slots.iter().map(|(slot, _)| slot).ne(slots.iter())
        || ordered_slots.first().map(|(_, value)| *value) != Some(U256::from(1))
    {
        return Err(NodError::InvalidEntryPriceSnapshot.into());
    }
    Ok(isos
        .iter()
        .copied()
        .zip(ordered_slots.iter().skip(1).map(|(_, price)| *price))
        .filter(|(_, price)| !price.is_zero())
        .collect())
}
