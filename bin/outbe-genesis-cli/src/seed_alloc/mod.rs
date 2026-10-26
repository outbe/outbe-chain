//! Validate reserved namespaces and install external contract allocations.

mod governments;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::GenesisError;
use crate::seed_codec::{
    address_bytes, hex32, is_protected_protocol_address, parse_i128, parse_u256, StorageBuilder,
    MARKER_CODE, STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
};

pub use self::governments::seed_governments_descriptor;

/// Reject genesis `alloc` entries that collide with the Stablecoin V1 namespace.
pub fn validate_stablecoin_namespace_alloc(
    alloc: &Map<String, Value>,
    require_reserved_markers: bool,
) -> Result<(), GenesisError> {
    let mut normalized: BTreeMap<String, (&str, &Map<String, Value>)> = BTreeMap::new();
    for (raw_address, account_val) in alloc {
        let norm_bytes = address_bytes(raw_address)?;
        let normalized_address = hex::encode(norm_bytes);
        if let Some(&(prev_raw, _)) = normalized.get(&normalized_address) {
            return Err(GenesisError::validation(format!(
                "duplicate genesis alloc address after normalization: {raw_address} and {prev_raw}"
            )));
        }
        let Some(account) = account_val.as_object() else {
            return Err(GenesisError::validation(format!(
                "genesis alloc {raw_address} must be an object"
            )));
        };
        if normalized_address.starts_with(STABLECOIN_ADDRESS_PREFIX) {
            return Err(GenesisError::validation(format!(
                "genesis alloc {raw_address} collides with reserved stablecoin prefix 0x{STABLECOIN_ADDRESS_PREFIX}"
            )));
        }
        normalized.insert(normalized_address, (raw_address.as_str(), account));
    }

    for reserved in [
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
    ] {
        if let Some(&(raw_address, account)) = normalized.get(reserved) {
            validate_reserved_account(raw_address, account, require_reserved_markers)?;
        } else if require_reserved_markers {
            return Err(GenesisError::validation(format!(
                "missing stablecoin reserved account 0x{reserved}"
            )));
        }
    }
    Ok(())
}

fn validate_reserved_account(
    raw_address: &str,
    account: &Map<String, Value>,
    require_reserved_markers: bool,
) -> Result<(), GenesisError> {
    if let Some(raw_code) = account.get("code").filter(|v| !v.is_null()) {
        let code_str = raw_code.as_str().unwrap_or("");
        if !code_str.eq_ignore_ascii_case(MARKER_CODE) {
            return Err(GenesisError::validation(format!(
                "stablecoin reserved account {raw_address} has conflicting code"
            )));
        }
    } else if require_reserved_markers {
        return Err(GenesisError::validation(format!(
            "stablecoin reserved account {raw_address} is missing marker code"
        )));
    }
    let has_nonempty_storage = match account.get("storage") {
        None | Some(Value::Null) => false,
        Some(Value::Object(m)) => !m.is_empty(),
        Some(_) => true,
    };
    if has_nonempty_storage {
        return Err(GenesisError::validation(format!(
            "stablecoin reserved account {raw_address} has conflicting storage"
        )));
    }
    validate_reserved_balance(raw_address, account)
}

fn validate_reserved_balance(
    raw_address: &str,
    account: &Map<String, Value>,
) -> Result<(), GenesisError> {
    for field in ["balance", "nonce"] {
        if let Some(val) = account.get(field).filter(|v| !v.is_null()) {
            let parsed = parse_u256(val).map_err(|_| {
                GenesisError::validation(format!(
                    "stablecoin reserved account {raw_address} has invalid {field}"
                ))
            })?;
            if !parsed.is_zero() {
                return Err(GenesisError::validation(format!(
                    "stablecoin reserved account {raw_address} has nonzero {field}"
                )));
            }
        }
    }
    Ok(())
}

/// Seed immutable CodeRegistry V1 capacity at slot 5.
pub fn seed_code_registry(
    storage: &mut StorageBuilder,
    config: &Value,
) -> Result<(), GenesisError> {
    let Some(obj) = config.as_object() else {
        return Err(GenesisError::validation(
            "code_registry must be a JSON object",
        ));
    };
    let configured = obj
        .get("max_repositories")
        .map(parse_i128)
        .transpose()?
        .unwrap_or(0);
    if !(configured == -1 || (1..=0xFFFF_FFFE).contains(&configured)) {
        return Err(GenesisError::validation(
            "code_registry.max_repositories must be -1 or in 1..=4294967294",
        ));
    }
    let maximum: u64 = if configured == -1 {
        0xFFFF_FFFF
    } else {
        configured as u64
    };
    storage.set_slot(5u64, maximum);
    Ok(())
}

/// Embed external contract bytecode and storage from `contracts_dir` into `alloc`.
pub fn seed_external_contracts(
    alloc: &mut Map<String, Value>,
    contracts_list: &[Value],
    contracts_dir: &Path,
) -> Result<(), GenesisError> {
    for entry_val in contracts_list {
        let Some(entry) = entry_val.as_object() else {
            return Err(GenesisError::validation("contract entry must be an object"));
        };
        seed_one_external_contract(alloc, entry, contracts_dir)?;
    }
    Ok(())
}

fn seed_one_external_contract(
    alloc: &mut Map<String, Value>,
    entry: &Map<String, Value>,
    contracts_dir: &Path,
) -> Result<(), GenesisError> {
    let raw_addr = entry
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("contract entry missing address"))?;
    let addr_norm = hex::encode(address_bytes(raw_addr)?);
    if is_protected_protocol_address(&addr_norm) {
        return Err(GenesisError::validation(format!(
            "contract {raw_addr} collides with a protocol-reserved address; refusing to overwrite protocol state"
        )));
    }
    let code_hex = load_contract_bytecode(entry, contracts_dir)?;
    let storage_entries = load_contract_storage(entry, contracts_dir)?;

    let target = alloc
        .entry(addr_norm.clone())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("alloc entry must be an object"))?;
    if target
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|existing| existing != MARKER_CODE && existing != code_hex)
    {
        return Err(GenesisError::validation(format!(
            "alloc entry {addr_norm} already has different non-marker code; refusing to overwrite"
        )));
    }
    let nonce = entry
        .get("nonce")
        .and_then(Value::as_str)
        .unwrap_or("0x1")
        .to_owned();
    let balance = entry
        .get("balance")
        .and_then(Value::as_str)
        .unwrap_or("0x0")
        .to_owned();
    target.insert("code".to_owned(), Value::String(code_hex));
    target.insert("nonce".to_owned(), Value::String(nonce));
    target.insert("balance".to_owned(), Value::String(balance));
    if !storage_entries.is_empty() {
        let target_storage = target
            .entry("storage")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or_else(|| GenesisError::validation("storage must be an object"))?;
        target_storage.extend(storage_entries);
    }
    Ok(())
}

fn load_contract_bytecode(
    entry: &Map<String, Value>,
    contracts_dir: &Path,
) -> Result<String, GenesisError> {
    let code_file = entry
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("contract entry missing code"))?;
    let code_path = contracts_dir.join(code_file);
    let code_hex = fs::read_to_string(&code_path)
        .map_err(|err| GenesisError::io(&code_path, err))?
        .trim()
        .to_owned();
    if code_hex.len() <= 2 || !code_hex.starts_with("0x") {
        return Err(GenesisError::validation(format!(
            "{}: empty or non-0x-prefixed bytecode",
            code_path.display()
        )));
    }
    Ok(code_hex)
}

fn load_contract_storage(
    entry: &Map<String, Value>,
    contracts_dir: &Path,
) -> Result<Map<String, Value>, GenesisError> {
    let mut storage_entries = Map::new();
    let Some(state_file) = entry.get("state").and_then(Value::as_str) else {
        return Ok(storage_entries);
    };
    let state_path = contracts_dir.join(state_file);
    let state_raw: Map<String, Value> = serde_json::from_str(
        &fs::read_to_string(&state_path).map_err(|err| GenesisError::io(&state_path, err))?,
    )?;
    for (slot_str, value_val) in state_raw {
        let slot_u256 = parse_u256(&Value::String(slot_str))?;
        let val_u256 = parse_u256(&value_val)?;
        storage_entries.insert(hex32(slot_u256), Value::String(hex32(val_u256)));
    }
    Ok(storage_entries)
}

/// Copy optional `seed.protocol_constants` into `genesis.config.outbeProtocol`.
pub fn seed_protocol_constants(genesis: &mut Value, seed: &Value) -> Result<(), GenesisError> {
    let Some(profile) = seed.get("protocol_constants") else {
        return Ok(());
    };
    if !profile.is_object() {
        return Err(GenesisError::validation(
            "seed protocol_constants must be a JSON object",
        ));
    }
    let config = genesis
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("genesis must be a JSON object"))?
        .entry("config")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("genesis config must be a JSON object"))?;
    if config
        .get("outbeProtocol")
        .is_some_and(|existing| existing != profile)
    {
        return Err(GenesisError::validation(
            "seed protocol_constants conflicts with genesis config.outbeProtocol",
        ));
    }
    config.insert("outbeProtocol".to_owned(), profile.clone());
    Ok(())
}
