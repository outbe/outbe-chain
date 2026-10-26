//! Seed protocol assets, markets and day-indexed economic state.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use serde_json::{Map, Value};

use crate::error::GenesisError;
use crate::keccak::keccak256;
use crate::seed_codec::{
    address_as_u256, address_bytes, alloc_balance_hex, data_slot, parse_u256, parse_u32, parse_u64,
    timestamp_to_utc_date_key, u32_bytes, StorageBuilder, METADOSIS_ADDRESS, ORACLE_ADDRESS,
    TRIBUTE_ADDRESS, VAULT_ROUTER_LIQUIDITY_SOURCES, VAULT_ROUTER_LIQUIDITY_TARGETS,
};
use crate::seed_oracle::seed_oracle;

/// Seed Cycle slot 2 with the UTC day owned by the genesis block.
pub fn seed_cycle(storage: &mut StorageBuilder, genesis_timestamp: u64) {
    storage.set_slot(2u64, timestamp_to_utc_date_key(genesis_timestamp));
}

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

/// Generate `tribute_token_id = keccak256(owner_20B ++ wwd_4B)`.
pub fn tribute_token_id(owner: &str, worldwide_day: u32) -> Result<[u8; 32], GenesisError> {
    let mut buf = [0u8; 24];
    buf[..20].copy_from_slice(&address_bytes(owner)?);
    buf[20..].copy_from_slice(&u32_bytes(worldwide_day));
    Ok(keccak256(&buf))
}

/// Compute `keccak256(day_4B ++ index_4B)` for tribute day index.
pub fn day_index_key(day: u32, index: u32) -> [u8; 32] {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&u32_bytes(day));
    buf[4..].copy_from_slice(&u32_bytes(index));
    keccak256(&buf)
}

/// Compute `keccak256(owner_20B ++ index_4B)` for tribute/nod owner index.
pub fn owner_index_key(owner: &str, index: u32) -> Result<[u8; 32], GenesisError> {
    let mut buf = [0u8; 24];
    buf[..20].copy_from_slice(&address_bytes(owner)?);
    buf[20..].copy_from_slice(&u32_bytes(index));
    Ok(keccak256(&buf))
}

/// Seed pre-existing Tribute records into Tribute storage.
pub fn seed_tributes(storage: &mut StorageBuilder, tributes: &[Value]) -> Result<(), GenesisError> {
    let mut day_counts: BTreeMap<u32, u32> = BTreeMap::new();
    let mut day_nominals: BTreeMap<u32, U256> = BTreeMap::new();
    let mut owner_counts: BTreeMap<String, u32> = BTreeMap::new();

    for tribute in tributes {
        let owner = tribute
            .get("owner")
            .and_then(Value::as_str)
            .ok_or_else(|| GenesisError::validation("tribute missing owner"))?;
        let wwd = tribute
            .get("worldwide_day")
            .ok_or_else(|| GenesisError::validation("tribute missing worldwide_day"))
            .and_then(parse_u32)?;
        let settlement = tribute
            .get("issuance_amount")
            .ok_or_else(|| GenesisError::validation("tribute missing issuance_amount"))
            .and_then(parse_u256)?;
        let currency = tribute
            .get("issuance_currency")
            .ok_or_else(|| GenesisError::validation("tribute missing issuance_currency"))
            .and_then(parse_u32)?;
        let nominal = tribute
            .get("nominal_amount")
            .ok_or_else(|| GenesisError::validation("tribute missing nominal_amount"))
            .and_then(parse_u256)?;

        let token_id = tribute_token_id(owner, wwd)?;
        storage.set_mapping(1, &token_id, address_as_u256(owner)?);
        storage.set_mapping(2, &token_id, wwd);
        storage.set_mapping(3, &token_id, settlement);
        storage.set_mapping(4, &token_id, currency);
        storage.set_mapping(5, &token_id, nominal);

        let day_idx = *day_counts.get(&wwd).unwrap_or(&0);
        let di_key = day_index_key(wwd, day_idx);
        storage.set_mapping_b256(9, &di_key, &token_id);
        day_counts.insert(wwd, day_idx + 1);

        let prev_nom = *day_nominals.get(&wwd).unwrap_or(&U256::ZERO);
        day_nominals.insert(wwd, prev_nom + nominal);

        let owner_lower = owner.to_ascii_lowercase();
        let oi = *owner_counts.get(&owner_lower).unwrap_or(&0);
        let oi_key = owner_index_key(owner, oi)?;
        storage.set_mapping_b256(11, &oi_key, &token_id);
        owner_counts.insert(owner_lower, oi + 1);
    }

    for (wwd, count) in day_counts {
        storage.set_mapping(6, &u32_bytes(wwd), count);
    }
    for (wwd, nominal_total) in day_nominals {
        storage.set_mapping(7, &u32_bytes(wwd), nominal_total);
    }
    for (owner, count) in owner_counts {
        storage.set_mapping(10, &address_bytes(&owner)?, count);
    }
    storage.set_slot(0u64, tributes.len());
    Ok(())
}

/// Initialize the Tribute `day_totals` DSL record for OFFERING days (`slot 1`).
pub fn seed_tribute_day_totals(storage: &mut StorageBuilder, days: &[u32]) {
    for &wwd in days {
        storage.set_mapping(1, &u32_bytes(wwd), 1u64);
    }
}

/// Seed Tribute worldwide-day records (slots 27..31) and `active_wwd` set (slots 32..33).
pub fn seed_tribute_worldwide_days(
    storage: &mut StorageBuilder,
    wwds: &[Value],
) -> Result<(), GenesisError> {
    const WWD_BASE: u64 = 27;
    const ACTIVE_WWD_BASE: u64 = 32;

    for (idx, entry) in wwds.iter().enumerate() {
        let wwd = entry
            .get("wwd")
            .ok_or_else(|| GenesisError::validation("worldwide_days entry missing wwd"))
            .and_then(parse_u32)?;
        let wwd_key = u32_bytes(wwd);
        let day = entry.get("day").map(parse_u32).transpose()?.unwrap_or(wwd);
        let forming_start = entry
            .get("forming_start")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(0);
        let forming_end = entry
            .get("forming_end")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(0);
        let offerings_start = entry
            .get("offerings_start")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(forming_end);
        let offering_end = entry
            .get("offering_end")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(0);

        storage.set_mapping(WWD_BASE, &wwd_key, day);
        storage.set_mapping(WWD_BASE + 1, &wwd_key, forming_start);
        storage.set_mapping(WWD_BASE + 2, &wwd_key, forming_end);
        storage.set_mapping(WWD_BASE + 3, &wwd_key, offerings_start);
        storage.set_mapping(WWD_BASE + 4, &wwd_key, offering_end);

        storage.set_slot(data_slot(ACTIVE_WWD_BASE) + U256::from(idx), wwd);
        storage.set_mapping(ACTIVE_WWD_BASE + 1, &wwd_key, idx + 1);
    }
    storage.set_slot(ACTIVE_WWD_BASE, wwds.len());
    Ok(())
}

/// Metadosis storage seeder (no direct storage words written; worldwide days live on Tribute).
pub fn seed_metadosis(_storage: &mut StorageBuilder, _config: &Value) {}

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

/// Seed VaultRouter default factory liquidity source/target registries.
pub fn seed_vault_router(storage: &mut StorageBuilder) -> Result<(), GenesisError> {
    seed_address_set_with_types(storage, 4, 6, &VAULT_ROUTER_LIQUIDITY_SOURCES)?;
    seed_address_set_with_types(storage, 7, 9, &VAULT_ROUTER_LIQUIDITY_TARGETS)?;
    Ok(())
}

/// Clear `seed.metadosis.worldwide_days` so block 1 creates the first `WorldwideDay`.
pub fn clear_seeded_metadosis_days(seed: &mut Value) -> Result<(), GenesisError> {
    let Some(metadosis) = seed.get_mut("metadosis") else {
        return Ok(());
    };
    let Some(obj) = metadosis.as_object_mut() else {
        return Err(GenesisError::validation(
            "seed metadosis must be a JSON object",
        ));
    };
    obj.insert("worldwide_days".to_owned(), Value::Array(Vec::new()));
    Ok(())
}

/// Retarget `metadosis.worldwide_days[].wwd` and `oracle.scurve_seeds[].peak_day` to `day`.
pub fn override_worldwide_day(seed: &mut Value, day: u32) {
    if let Some(wwds) = seed
        .pointer_mut("/metadosis/worldwide_days")
        .and_then(Value::as_array_mut)
    {
        for w in wwds {
            if let Some(obj) = w.as_object_mut() {
                obj.insert("wwd".to_owned(), Value::Number(day.into()));
            }
        }
    }
    if let Some(scurves) = seed
        .pointer_mut("/oracle/scurve_seeds")
        .and_then(Value::as_array_mut)
    {
        for s in scurves {
            if let Some(obj) = s.as_object_mut() {
                obj.insert("peak_day".to_owned(), Value::Number(day.into()));
            }
        }
    }
}

/// Seed Tribute, Metadosis, and Oracle sections into `alloc`.
pub fn seed_economic_state(
    seed: &Value,
    alloc: &mut Map<String, Value>,
) -> Result<(), GenesisError> {
    if let Some(tributes) = seed.get("tributes").and_then(Value::as_array) {
        let mut tribute_storage = StorageBuilder::new();
        seed_tributes(&mut tribute_storage, tributes)?;
        let offering_days = collect_offering_days(seed)?;
        seed_tribute_day_totals(&mut tribute_storage, &offering_days);
        merge_storage_into_alloc(alloc, TRIBUTE_ADDRESS, &tribute_storage)?;
    }

    if let Some(metadosis) = seed.get("metadosis") {
        seed_metadosis_section(metadosis, alloc)?;
    }

    if let Some(oracle) = seed.get("oracle") {
        let mut oracle_storage = StorageBuilder::new();
        seed_oracle(&mut oracle_storage, oracle)?;
        merge_storage_into_alloc(alloc, ORACLE_ADDRESS, &oracle_storage)?;
    }
    Ok(())
}

fn collect_offering_days(seed: &Value) -> Result<Vec<u32>, GenesisError> {
    let Some(wwds) = seed
        .pointer("/metadosis/worldwide_days")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut offering_days = Vec::new();
    for entry_wd in wwds {
        let status = entry_wd.get("status").and_then(Value::as_u64).unwrap_or(0);
        if status == 2 {
            if let Some(wwd_val) = entry_wd.get("wwd") {
                offering_days.push(parse_u32(wwd_val)?);
            }
        }
    }
    Ok(offering_days)
}

fn seed_metadosis_section(
    metadosis: &Value,
    alloc: &mut Map<String, Value>,
) -> Result<(), GenesisError> {
    let mut meta_storage = StorageBuilder::new();
    seed_metadosis(&mut meta_storage, metadosis);
    merge_storage_into_alloc(alloc, METADOSIS_ADDRESS, &meta_storage)?;

    let Some(wwds) = metadosis.get("worldwide_days").and_then(Value::as_array) else {
        return Ok(());
    };
    if !wwds.is_empty() {
        let mut tribute_wwd_storage = StorageBuilder::new();
        seed_tribute_worldwide_days(&mut tribute_wwd_storage, wwds)?;
        merge_storage_into_alloc(alloc, TRIBUTE_ADDRESS, &tribute_wwd_storage)?;
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
