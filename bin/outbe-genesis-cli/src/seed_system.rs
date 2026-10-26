//! Seed governance and system precompile state.

use std::fs;
use std::path::Path;

use alloy_primitives::U256;
use serde_json::{Map, Value, json};

use crate::error::GenesisError;
use crate::keccak::keccak256;
use crate::network::default_canon_dir;
use crate::seed_alloc::{seed_code_registry, seed_governments_descriptor};
use crate::seed_codec::{
    ACCOUNTING_PROGRESS_ADDRESS, CODE_REGISTRY_ADDRESS, GOVERNANCE_ADDRESS, MARKER_CODE,
    StorageBuilder, TEE_REGISTRY_ADDRESS, ZEROFEE_ADDRESS, compute_tee_policy_hash, parse_u64,
    u64_bytes, write_storage_bytes,
};
use crate::seed_economy::merge_storage_into_alloc;

#[derive(Clone, Copy)]
struct GovernanceTextSlots {
    text_slot: u64,
    version_slot: u64,
    hash_slot: u64,
    revisions_base: u64,
}

const META_CANON_SLOTS: GovernanceTextSlots = GovernanceTextSlots {
    text_slot: 0,
    version_slot: 1,
    hash_slot: 2,
    revisions_base: 3,
};

const CANON_SLOTS: GovernanceTextSlots = GovernanceTextSlots {
    text_slot: 4,
    version_slot: 5,
    hash_slot: 6,
    revisions_base: 7,
};

/// Seed the genesis TEE attestation policy (`TeeRegistry` slot 2 + `config.teePolicy`) when `tee_policy` is present.
pub fn seed_tee_policy(
    genesis: &mut Value,
    alloc: &mut Map<String, Value>,
    seed: &Value,
) -> Result<(), GenesisError> {
    let Some(policy) = seed.get("tee_policy").and_then(Value::as_object) else {
        return Ok(());
    };
    if policy.is_empty() {
        return Ok(());
    }
    let allowed_mrsigner = extract_string_list(policy.get("allowed_mrsigner"));
    let allowed_mrenclave = extract_string_list(policy.get("allowed_mrenclave"));
    let min_isv_svn = policy
        .get("min_isv_svn")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(0) as u16;
    let policy_hash = compute_tee_policy_hash(&allowed_mrsigner, &allowed_mrenclave, min_isv_svn)?;

    let mut storage = StorageBuilder::new();
    storage.set_raw_slot_hex(2u64, &format!("0x{}", hex::encode(policy_hash)))?;
    let entry = alloc
        .entry(TEE_REGISTRY_ADDRESS.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("alloc entry must be an object"))?;
    entry.insert("code".to_owned(), Value::String(MARKER_CODE.to_owned()));
    entry
        .entry("balance")
        .or_insert_with(|| Value::String("0x0".to_owned()));
    merge_storage_into_alloc(alloc, TEE_REGISTRY_ADDRESS, &storage)?;

    let config = genesis
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("genesis must be an object"))?
        .entry("config")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation("genesis.config must be an object"))?;
    config.insert(
        "teePolicy".to_owned(),
        json!({
            "allowed_mrsigner": allowed_mrsigner,
            "allowed_mrenclave": allowed_mrenclave,
            "min_isv_svn": min_isv_svn,
        }),
    );
    Ok(())
}

fn extract_string_list(val: Option<&Value>) -> Vec<String> {
    val.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Seed ZeroFee paymaster schema version at slot 0 (`1`).
pub fn seed_zerofee(storage: &mut StorageBuilder) {
    storage.set_slot(0u64, 1u64);
}

/// Seed AccountingProgress `last_accounted_block_number` at slot 0 (`0`).
pub fn seed_accounting_progress(storage: &mut StorageBuilder) {
    storage.set_slot(0u64, 0u64);
}

/// Seed Governance canon and meta-canon texts from `canon_dir/{metacanon.md,canon.md}` when present.
pub fn seed_governance(
    storage: &mut StorageBuilder,
    canon_dir: Option<&Path>,
) -> Result<(bool, bool), GenesisError> {
    let mut meta_seeded = false;
    let mut canon_seeded = false;
    if let Some(dir) = canon_dir.filter(|d| d.is_dir()) {
        let meta_path = dir.join("metacanon.md");
        if meta_path.is_file() {
            let data = fs::read(&meta_path).map_err(|err| GenesisError::io(&meta_path, err))?;
            seed_governance_text(storage, META_CANON_SLOTS, &data)?;
            meta_seeded = true;
        }
        let canon_path = dir.join("canon.md");
        if canon_path.is_file() {
            let data = fs::read(&canon_path).map_err(|err| GenesisError::io(&canon_path, err))?;
            seed_governance_text(storage, CANON_SLOTS, &data)?;
            canon_seeded = true;
        }
    }
    Ok((meta_seeded, canon_seeded))
}

fn seed_governance_text(
    storage: &mut StorageBuilder,
    slots: GovernanceTextSlots,
    data: &[u8],
) -> Result<(), GenesisError> {
    write_storage_bytes(storage, U256::from(slots.text_slot), data)?;
    storage.set_slot(slots.version_slot, 1u64);
    let h = keccak256(data);
    storage.set_raw_slot_hex(slots.hash_slot, &format!("0x{}", hex::encode(h)))?;
    storage.set_mapping_b256(slots.revisions_base, &u64_bytes(1), &h);
    Ok(())
}

/// Seed Governance, AccountingProgress, CodeRegistry, ZeroFee, and TeeRegistry state.
pub fn seed_system_state(
    genesis: &mut Value,
    seed: &Value,
    alloc: &mut Map<String, Value>,
    canon_dir: Option<&str>,
) -> Result<(), GenesisError> {
    let default_canon = default_canon_dir();
    let resolved_canon = canon_dir.map_or(default_canon.as_path(), Path::new);
    let mut governance_storage = StorageBuilder::new();
    seed_governance(&mut governance_storage, Some(resolved_canon))?;
    if !governance_storage.entries.is_empty() {
        merge_storage_into_alloc(alloc, GOVERNANCE_ADDRESS, &governance_storage)?;
    }

    let mut accounting_storage = StorageBuilder::new();
    seed_accounting_progress(&mut accounting_storage);
    merge_storage_into_alloc(alloc, ACCOUNTING_PROGRESS_ADDRESS, &accounting_storage)?;

    if let Some(code_reg_cfg) = seed.get("code_registry") {
        let mut code_registry_storage = StorageBuilder::new();
        seed_code_registry(&mut code_registry_storage, code_reg_cfg)?;
        seed_governments_descriptor(genesis, &mut code_registry_storage, code_reg_cfg)?;
        merge_storage_into_alloc(alloc, CODE_REGISTRY_ADDRESS, &code_registry_storage)?;
    }

    let mut zerofee_storage = StorageBuilder::new();
    seed_zerofee(&mut zerofee_storage);
    merge_storage_into_alloc(alloc, ZEROFEE_ADDRESS, &zerofee_storage)?;

    seed_tee_policy(genesis, alloc, seed)?;
    Ok(())
}
