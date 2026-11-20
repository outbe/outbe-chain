//! Seed protocol assets and markets.

use alloy_primitives::U256;
use serde_json::{Map, Value};

use crate::error::GenesisError;
use crate::seed_codec::{
    address_as_u256, address_bytes, alloc_balance_hex, data_slot, parse_u256, StorageBuilder,
    ORACLE_ADDRESS, SECONDS_PER_DAY, VAULT_ROUTER_STABLECOIN_SOURCES,
    VAULT_ROUTER_STABLECOIN_TARGETS,
};
use crate::seed_oracle::seed_oracle;

/// Seed native EVM token balances (`alloc[addr].balance`).
pub fn seed_coen(
    alloc: &mut Map<String, Value>,
    balances: &Map<String, Value>,
) -> Result<(), GenesisError> {
    for (addr, amount_val) in balances {
        let amount = parse_u256(amount_val)?;
        let addr_hex = hex::encode(address_bytes(addr)?);
        let entry = alloc
            .entry(addr_hex)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| GenesisError::validation("alloc entry must be an object"))?;
        entry.insert(
            "balance".to_owned(),
            Value::String(alloc_balance_hex(amount)),
        );
    }
    Ok(())
}

/// Seed an OZ-style enumerable `Set<Address>` plus its parallel `mapping(Address => u8)` type map.
pub fn seed_address_set_with_types(
    storage: &mut StorageBuilder,
    set_base: u64,
    type_map_slot: u64,
    entries: &[(&str, u64)],
) -> Result<(), GenesisError> {
    for (idx, &(addr, type_value)) in entries.iter().enumerate() {
        let addr_bytes = address_bytes(addr)?;
        storage.set_slot(
            data_slot(set_base) + U256::from(idx),
            address_as_u256(addr)?,
        );
        storage.set_mapping(set_base + 1, &addr_bytes, idx + 1);
        storage.set_mapping(type_map_slot, &addr_bytes, type_value);
    }
    storage.set_slot(set_base, entries.len());
    Ok(())
}

/// Seed VaultRouter default factory stablecoin source/target registries.
pub fn seed_vault_router(storage: &mut StorageBuilder) -> Result<(), GenesisError> {
    seed_address_set_with_types(storage, 4, 6, &VAULT_ROUTER_STABLECOIN_SOURCES)?;
    seed_address_set_with_types(storage, 7, 9, &VAULT_ROUTER_STABLECOIN_TARGETS)?;
    Ok(())
}

/// 00:00 UTC of the first Worldwide Day the chain makes, in Unix seconds: the
/// start hour of each seeded S-curve.
fn first_worldwide_day_start(genesis_timestamp: u64) -> u64 {
    genesis_timestamp.saturating_sub(12 * 60 * 60) / SECONDS_PER_DAY * SECONDS_PER_DAY
}

/// Seed the Oracle section into `alloc`. `genesis_timestamp` is the header timestamp.
pub fn seed_economic_state(
    seed: &Value,
    genesis_timestamp: Option<u64>,
    alloc: &mut Map<String, Value>,
) -> Result<(), GenesisError> {
    if let Some(oracle) = seed.get("oracle") {
        let mut oracle_storage = StorageBuilder::new();
        seed_oracle(
            &mut oracle_storage,
            oracle,
            genesis_timestamp.map(first_worldwide_day_start),
        )?;
        merge_storage_into_alloc(alloc, ORACLE_ADDRESS, &oracle_storage)?;
    }
    Ok(())
}

pub(crate) fn merge_storage_into_alloc(
    alloc: &mut Map<String, Value>,
    address: &str,
    builder: &StorageBuilder,
) -> Result<(), GenesisError> {
    let entry = alloc
        .entry(address.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("alloc entry must be an object"))?;
    let storage = entry
        .entry("storage")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("storage must be an object"))?;
    for (k, v) in &builder.entries {
        storage.insert(k.clone(), Value::String(v.clone()));
    }
    Ok(())
}
