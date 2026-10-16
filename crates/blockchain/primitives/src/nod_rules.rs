//! Pure rules shared by Lysis and Nod issuance.

use alloy_primitives::{keccak256, B256, U256};

use crate::{
    error::Result, math::scaled_math::checked_mul_div_floor, storage::types::StorageKey,
    time::WorldwideDay, units::SCALE_1E6_U256,
};

pub fn bucket_key(day: WorldwideDay, entry: U256, currency: u16) -> B256 {
    let mut bytes = [0u8; 38];
    bytes[..4].copy_from_slice(day.key_bytes().as_slice());
    bytes[4..36].copy_from_slice(&entry.to_be_bytes::<32>());
    bytes[36..].copy_from_slice(&currency.to_be_bytes());
    keccak256(bytes)
}

pub fn is_issuable_entry(entry: U256) -> bool {
    crate::markup::marked_up(entry, u16::MAX).is_some()
}

pub fn settlement_cost_minor(entry: U256, load: U256) -> Result<U256> {
    let cost = checked_mul_div_floor(entry, load, SCALE_1E6_U256)?;
    Ok(if !entry.is_zero() && !load.is_zero() {
        cost.max(U256::ONE)
    } else {
        cost
    })
}
