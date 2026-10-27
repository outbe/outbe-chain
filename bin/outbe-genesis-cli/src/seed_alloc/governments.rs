//! Pre-genesis `OGD1` Radicle governments descriptor seeding.

use std::collections::BTreeSet;

use alloy_primitives::U256;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::error::GenesisError;
use crate::network::{DEFAULT_DKG_PREPARE_WINDOW_BLOCKS, DEFAULT_EPOCH_LENGTH_BLOCKS};
use crate::seed_codec::{parse_header_timestamp, parse_i128, write_storage_bytes, StorageBuilder};

fn parse_governments_description(
    governments: &Map<String, Value>,
) -> Result<Vec<u8>, GenesisError> {
    let expected: BTreeSet<&str> = [
        "description",
        "initial_content_manifest_sha256",
        "initial_tree_oid",
    ]
    .into_iter()
    .collect();
    let actual: BTreeSet<&str> = governments.keys().map(String::as_str).collect();
    if actual != expected {
        return Err(GenesisError::validation(
            "code_registry.governments requires exactly description, initial_content_manifest_sha256, initial_tree_oid",
        ));
    }
    let description = governments
        .get("description")
        .and_then(Value::as_str)
        .filter(|s| !s.contains('\r') && !s.contains('\0'))
        .ok_or_else(|| GenesisError::validation("invalid governments description"))?;
    let bytes = description.as_bytes().to_vec();
    if bytes.len() > 256 {
        return Err(GenesisError::validation(
            "governments description exceeds 256 UTF-8 bytes",
        ));
    }
    Ok(bytes)
}

fn parse_governments_commitment(
    governments: &Map<String, Value>,
    field: &str,
    size: usize,
) -> Result<Vec<u8>, GenesisError> {
    let hex_body = governments
        .get(field)
        .and_then(Value::as_str)
        .and_then(|v| v.strip_prefix("0x"))
        .filter(|h| h.len() == size * 2)
        .ok_or_else(|| {
            GenesisError::validation(format!("governments {field} must contain {size} hex bytes"))
        })?;
    let raw = hex::decode(hex_body)
        .map_err(|_| GenesisError::validation(format!("invalid governments {field}")))?;
    if raw.iter().all(|&b| b == 0) {
        return Err(GenesisError::validation(format!(
            "governments {field} must be nonzero"
        )));
    }
    Ok(raw)
}

fn validate_governments_chain_timing(chain_obj: &Map<String, Value>) -> Result<u64, GenesisError> {
    let chain_id_val = chain_obj
        .get("chainId")
        .ok_or_else(|| GenesisError::validation("missing config.chainId"))?;
    let chain_id_raw = parse_i128(chain_id_val)?;
    let chain_id = u64::try_from(chain_id_raw)
        .map_err(|_| GenesisError::validation("governments chain ID/timestamp must fit u64"))?;
    let epoch = chain_obj
        .get("epochLengthBlocks")
        .map(parse_i128)
        .transpose()?
        .unwrap_or_else(|| i128::from(DEFAULT_EPOCH_LENGTH_BLOCKS));
    let prepare = chain_obj
        .get("dkgPrepareWindowBlocks")
        .map(parse_i128)
        .transpose()?
        .unwrap_or_else(|| i128::from(DEFAULT_DKG_PREPARE_WINDOW_BLOCKS));
    let effective_prepare = prepare.min(epoch);
    if prepare.min(epoch - effective_prepare - 64) < 0 {
        return Err(GenesisError::validation(
            "governments requires at least 64 blocks before the next DKG freeze",
        ));
    }
    Ok(chain_id)
}

/// Commit the pre-genesis `OGD1` artifact to CodeRegistry slots 10 and 11 and `config.radicleGovernments`.
pub fn seed_governments_descriptor(
    genesis: &mut Value,
    storage: &mut StorageBuilder,
    config: &Value,
) -> Result<(), GenesisError> {
    let Some(gov_val) = config.get("governments") else {
        return Ok(());
    };
    let Some(governments) = gov_val.as_object() else {
        return Err(GenesisError::validation(
            "code_registry.governments requires exactly description, initial_content_manifest_sha256, initial_tree_oid",
        ));
    };
    let description_bytes = parse_governments_description(governments)?;
    let timestamp = parse_header_timestamp(genesis)?;
    let chain_obj = genesis
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("genesis must be a JSON object"))?
        .entry("config")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("genesis config must be a JSON object"))?;

    let chain_id = validate_governments_chain_timing(chain_obj)?;
    let tree_oid = parse_governments_commitment(governments, "initial_tree_oid", 20)?;
    let manifest_sha256 =
        parse_governments_commitment(governments, "initial_content_manifest_sha256", 32)?;

    let mut descriptor = Vec::with_capacity(97 + description_bytes.len());
    descriptor.extend_from_slice(b"OGD1");
    descriptor.extend_from_slice(&1u16.to_be_bytes());
    descriptor.extend_from_slice(&chain_id.to_be_bytes());
    descriptor.extend_from_slice(&timestamp.to_be_bytes());
    descriptor.extend_from_slice(&11u16.to_be_bytes());
    descriptor.extend_from_slice(b"governments");
    descriptor.extend_from_slice(&4u16.to_be_bytes());
    descriptor.extend_from_slice(b"main");
    descriptor.extend_from_slice(&(description_bytes.len() as u16).to_be_bytes());
    descriptor.extend_from_slice(&description_bytes);
    descriptor.extend_from_slice(&tree_oid);
    descriptor.extend_from_slice(&manifest_sha256);
    descriptor.extend_from_slice(b"\x01\x01");

    let encoded = format!("0x{}", hex::encode(&descriptor));
    if chain_obj
        .get("radicleGovernments")
        .and_then(Value::as_str)
        .is_some_and(|existing| existing != encoded)
    {
        return Err(GenesisError::validation(
            "conflicting radicleGovernments genesis descriptor",
        ));
    }
    let digest = Sha256::digest(&descriptor);
    storage.set_raw_slot_hex(10u64, &format!("0x{}", hex::encode(digest)))?;
    write_storage_bytes(storage, U256::from(11u64), &descriptor)?;
    chain_obj.insert("radicleGovernments".to_owned(), Value::String(encoded));
    Ok(())
}
