//! Pure rules shared by Lysis and Nod issuance.

use alloy_primitives::{keccak256, B256, U256};

use crate::{
    error::Result,
    settlement::{settlement_cost_units, SettlementCost},
    storage::types::StorageKey,
    time::WorldwideDay,
    units::PROTOCOL_AMOUNT_DECIMALS,
};

pub fn group_key(day: WorldwideDay, entry: U256, currency: u16) -> B256 {
    let mut bytes = [0u8; 38];
    bytes[..4].copy_from_slice(day.key_bytes().as_slice());
    bytes[4..36].copy_from_slice(&entry.to_be_bytes::<32>());
    bytes[36..].copy_from_slice(&currency.to_be_bytes());
    keccak256(bytes)
}

pub fn is_issuable_entry(entry: U256) -> bool {
    crate::markup::marked_up(entry, u16::MAX).is_some()
}

/// One Nod's cost in reference-currency minor units, by the settlement cost rule.
pub fn settlement_cost_minor(entry: U256, load: U256) -> Result<U256> {
    let cost = SettlementCost {
        entry_price_minor: entry,
        load_minor: load,
        units: U256::ONE,
        cost_rate_pct: 100,
    };
    Ok(settlement_cost_units(
        &cost,
        None,
        PROTOCOL_AMOUNT_DECIMALS,
    )?)
}
