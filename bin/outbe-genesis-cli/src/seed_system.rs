//! Seed system precompile state.

use serde_json::{json, Map, Value};

use crate::error::GenesisError;
use crate::seed_alloc::{seed_code_registry, seed_governments_descriptor};
use crate::seed_codec::{
    address_bytes, compute_tee_policy_hash, parse_u64, StorageBuilder, ACCOUNTING_PROGRESS_ADDRESS,
    ATTESTERS_REGISTRY_ADDRESS, CODE_REGISTRY_ADDRESS, ENCLAVE_REGISTRY_ADDRESS,
    FS_EPOCH_REGISTRY_ADDRESS, MARKER_CODE, SPENDING_UNIT_V2_ADDRESS, TEE_REGISTRY_ADDRESS,
    ZEROFEE_ADDRESS,
};
use crate::seed_economy::merge_storage_into_alloc;

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

/// Seed AccountingProgress, CodeRegistry, ZeroFee, and TeeRegistry state.
pub fn seed_system_state(
    genesis: &mut Value,
    seed: &Value,
    alloc: &mut Map<String, Value>,
) -> Result<(), GenesisError> {
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
    seed_pso_l2(seed, alloc)?;
    Ok(())
}

fn address_word_hex(addr_hex: &str) -> Result<String, GenesisError> {
    let bytes = address_bytes(addr_hex)?;
    if bytes == [0u8; 20] {
        return Err(GenesisError::validation("pso_l2 admin must be non-zero"));
    }
    Ok(format!("0x{:0>64}", hex::encode(bytes)))
}

/// Seed optional `pso_l2` genesis state for `AttestersRegistry`, `EnclaveRegistry`,
/// `FsEpochRegistry`, and `SpendingUnitV2` (`0x5200..` precompile range).
pub fn seed_pso_l2(seed: &Value, alloc: &mut Map<String, Value>) -> Result<(), GenesisError> {
    let Some(cfg) = seed.get("pso_l2").and_then(Value::as_object) else {
        return Ok(());
    };
    if cfg.is_empty() {
        return Ok(());
    }

    let admin_str = cfg
        .get("admin")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("pso_l2.admin must be a hex address"))?;
    let admin_word = address_word_hex(admin_str)?;
    let devnet_admin_register = cfg
        .get("devnet_admin_register")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let retiring_grace = cfg
        .get("retiring_grace")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(7200);
    let max_schema = cfg
        .get("max_schema")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(6);
    let min_registration_version = cfg
        .get("min_registration_version")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(2);

    let attesters_word = address_word_hex(ATTESTERS_REGISTRY_ADDRESS)?;
    let enclave_word = address_word_hex(ENCLAVE_REGISTRY_ADDRESS)?;
    let fs_epoch_word = address_word_hex(FS_EPOCH_REGISTRY_ADDRESS)?;

    // AttestersRegistry: slot 0 = admin
    let mut attesters_storage = StorageBuilder::new();
    attesters_storage.set_raw_slot_hex(0u64, &admin_word)?;
    merge_storage_into_alloc(alloc, ATTESTERS_REGISTRY_ADDRESS, &attesters_storage)?;

    // FsEpochRegistry: slot 0 = admin
    let mut fs_storage = StorageBuilder::new();
    fs_storage.set_raw_slot_hex(0u64, &admin_word)?;
    merge_storage_into_alloc(alloc, FS_EPOCH_REGISTRY_ADDRESS, &fs_storage)?;

    // EnclaveRegistry: slot 0 = admin, slot 2 = attesters_registry, slot 3 = retiring_grace,
    // slot 4 = max_schema, slot 5 = devnet_admin_register, slot 6 = min_registration_version
    let mut enclave_storage = StorageBuilder::new();
    enclave_storage.set_raw_slot_hex(0u64, &admin_word)?;
    enclave_storage.set_raw_slot_hex(2u64, &attesters_word)?;
    enclave_storage.set_slot(3u64, retiring_grace);
    enclave_storage.set_slot(4u64, max_schema);
    enclave_storage.set_slot(5u64, u64::from(devnet_admin_register));
    enclave_storage.set_slot(6u64, min_registration_version);
    merge_storage_into_alloc(alloc, ENCLAVE_REGISTRY_ADDRESS, &enclave_storage)?;

    // SpendingUnitV2: slot 0 = owner, slot 2 = attester_registry,
    // slot 3 = enclave_registry, slot 4 = fs_epoch_registry
    let mut su_storage = StorageBuilder::new();
    su_storage.set_raw_slot_hex(0u64, &admin_word)?;
    su_storage.set_raw_slot_hex(2u64, &attesters_word)?;
    su_storage.set_raw_slot_hex(3u64, &enclave_word)?;
    su_storage.set_raw_slot_hex(4u64, &fs_epoch_word)?;
    merge_storage_into_alloc(alloc, SPENDING_UNIT_V2_ADDRESS, &su_storage)?;

    Ok(())
}
