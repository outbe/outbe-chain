//! Build the base genesis and apply a declarative seed profile.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use crate::error::GenesisError;
use crate::inputs::{SeedInputs, SeedOptions};
use crate::launch_bundle;
use crate::network::{
    base_profile_path, deep_merge, default_canon_dir, default_contracts_dir, profiles_dir,
    DEFAULT_CHAIN_ID, DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS, DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
    DEFAULT_EPOCH_LENGTH_BLOCKS, DEFAULT_GAS_LIMIT, DEFAULT_PREFUND_COEN_UNITS, MAINNET_CHAIN_ID,
    MAX_GENESIS_AGE_SECONDS, SEED_SECTIONS,
};
use crate::seed_codec::{parse_u256, parse_u64, timestamp_to_utc_date_key};
use crate::seed_genesis::apply_seed;
use crate::yaml::load_yaml;

/// Build the base Reth genesis structure from `config`.
pub fn build_base_genesis(config: &Value) -> Result<Value, GenesisError> {
    let now = current_unix_timestamp();
    let timestamp = match config.get("timestamp") {
        Some(val) => parse_u64(val)?,
        None => now,
    };
    let age = now as i64 - timestamp as i64;
    let allow_stale = config
        .get("allow_stale_timestamp")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if age > MAX_GENESIS_AGE_SECONDS && !allow_stale {
        return Err(GenesisError::validation(format!(
            "`timestamp: {timestamp}` is {}h in the past. The TEE lease \
             runs from the genesis timestamp, so block 1 would fail with \
             'requested lease is already expired'. Drop the key to stamp now, or \
             set `allow_stale_timestamp: true` to reproduce an existing genesis.",
            age.div_euclid(3600)
        )));
    }

    let genesis_time = format_utc_iso8601(timestamp)?;
    let chain_id = u64_or_default(config, "chain_id", DEFAULT_CHAIN_ID)?;
    let epoch_length_blocks =
        u64_or_default(config, "epoch_length_blocks", DEFAULT_EPOCH_LENGTH_BLOCKS)?;
    let dkg_prepare_window_blocks = u64_or_default(
        config,
        "dkg_prepare_window_blocks",
        DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
    )?;
    let dkg_activation_grace_blocks = u64_or_default(
        config,
        "dkg_activation_grace_blocks",
        DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS,
    )?;
    let gas_limit = match config.get("gas_limit") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => DEFAULT_GAS_LIMIT.to_owned(),
    };

    Ok(json!({
        "config": {
            "chainId": chain_id,
            "genesisTime": genesis_time,
            "homesteadBlock": 0,
            "eip150Block": 0,
            "eip155Block": 0,
            "eip158Block": 0,
            "byzantiumBlock": 0,
            "constantinopleBlock": 0,
            "petersburgBlock": 0,
            "istanbulBlock": 0,
            "berlinBlock": 0,
            "londonBlock": 0,
            "mergeNetsplitBlock": 0,
            "terminalTotalDifficulty": 0,
            "terminalTotalDifficultyPassed": true,
            "shanghaiTime": 0,
            "cancunTime": 0,
            "pragueTime": 0,
            "epochLengthBlocks": epoch_length_blocks,
            "dkgPrepareWindowBlocks": dkg_prepare_window_blocks,
            "dkgActivationGraceBlocks": dkg_activation_grace_blocks
        },
        "nonce": "0x0",
        "timestamp": format!("0x{timestamp:x}"),
        "extraData": "0x",
        "gasLimit": gas_limit,
        "difficulty": "0x0",
        "mixHash": format!("0x{}", "00".repeat(32)),
        "coinbase": format!("0x{}", "00".repeat(20)),
        "alloc": {}
    }))
}

fn u64_or_default(config: &Value, key: &str, default: u64) -> Result<u64, GenesisError> {
    match config.get(key) {
        Some(val) => parse_u64(val),
        None => Ok(default),
    }
}

/// Prefund founding validators in `genesis["alloc"]`.
pub fn prefund_validators(
    genesis: &mut Value,
    config: &Value,
    validators: &[Value],
) -> Result<(), GenesisError> {
    let prefund = match config.get("prefund_coen_units") {
        Some(val) => parse_u256(val)?,
        None => U256::from(DEFAULT_PREFUND_COEN_UNITS),
    };
    if prefund.is_zero() {
        return Ok(());
    }
    let Some(genesis_obj) = genesis.as_object_mut() else {
        return Err(GenesisError::validation("genesis must be a JSON object"));
    };
    let alloc = genesis_obj
        .entry("alloc".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(alloc_obj) = alloc.as_object_mut() else {
        return Err(GenesisError::validation(
            "genesis.alloc must be a JSON object",
        ));
    };

    let balance_hex = format!("0x{prefund:x}");
    for validator in validators {
        let Some(address) = validator.get("address").and_then(Value::as_str) else {
            return Err(GenesisError::validation("validator missing address"));
        };
        let entry = alloc_obj
            .entry(address.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(entry_obj) = entry.as_object_mut() {
            entry_obj
                .entry("balance".to_owned())
                .or_insert_with(|| Value::String(balance_hex.clone()));
        }
    }
    Ok(())
}

/// Build the protocol seed sections by merging the network's overrides on top of `testnet.yaml`.
pub fn build_seed(config: &Value) -> Result<Value, GenesisError> {
    let baseline = load_yaml(&base_profile_path())?;
    let is_mainnet = config.get("chain_id").and_then(Value::as_u64) == Some(MAINNET_CHAIN_ID)
        || config.get("network").and_then(Value::as_str) == Some("mainnet");
    let mut seed_map = Map::new();
    for section in SEED_SECTIONS {
        if is_mainnet && section == "protocol_constants" {
            continue;
        }
        if let Some(val) = baseline.get(section) {
            seed_map.insert(section.to_owned(), val.clone());
        }
    }
    for section in SEED_SECTIONS {
        if let Some(override_val) = config.get(section) {
            let merged = deep_merge(seed_map.get(section), override_val);
            seed_map.insert(section.to_owned(), merged);
        }
    }
    if let Some(top_min_stake) = config.get("min_stake") {
        let staking_entry = seed_map
            .entry("staking".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(staking_obj) = staking_entry.as_object_mut() {
            if config.pointer("/staking/min_stake").is_none() {
                staking_obj.insert("min_stake".to_owned(), top_min_stake.clone());
            }
        }
    }
    let mut seed = Value::Object(seed_map);
    add_feeder_pairs(&mut seed, config)?;
    Ok(seed)
}

fn add_feeder_pairs(seed: &mut Value, config: &Value) -> Result<(), GenesisError> {
    let Some(table) = launch_bundle::feeder_pairs_table(config)? else {
        return Ok(());
    };
    let Some(seed_obj) = seed.as_object_mut() else {
        return Ok(());
    };
    let oracle = seed_obj
        .entry("oracle".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(oracle_obj) = oracle.as_object_mut() else {
        return Ok(());
    };

    merge_feeder_currency_pairs(oracle_obj, &table);
    append_feeder_seed_arrays(oracle_obj, config)
}

fn merge_feeder_currency_pairs(oracle_obj: &mut Map<String, Value>, table: &Value) {
    let pairs_val = oracle_obj
        .entry("pairs".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(pairs_arr) = pairs_val.as_array_mut() else {
        return;
    };
    let mut listed: BTreeSet<(String, String)> = pairs_arr
        .iter()
        .filter_map(|pair| {
            let base = pair.get("base")?.as_str()?.to_owned();
            let quote = match pair.get("quote")? {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some((base, quote))
        })
        .collect();
    let Some(currency_pairs) = table.get("currency_pairs").and_then(Value::as_array) else {
        return;
    };
    for pair in currency_pairs {
        let Some(base) = pair.get("base").and_then(Value::as_str) else {
            continue;
        };
        let Some(quote) = pair.get("quote").and_then(Value::as_str) else {
            continue;
        };
        if listed.insert((base.to_owned(), quote.to_owned())) {
            pairs_arr.push(json!({ "base": base, "quote": quote }));
        }
    }
}

fn append_feeder_seed_arrays(
    oracle_obj: &mut Map<String, Value>,
    config: &Value,
) -> Result<(), GenesisError> {
    let Some(feeder_seeds_rel) = config
        .get("feeder_seeds")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let seeds = load_yaml(&profiles_dir().join(feeder_seeds_rel))?;
    for key in ["initial_rates", "scurve_seeds"] {
        let mut combined = oracle_obj
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(extra) = seeds.get(key).and_then(Value::as_array) {
            combined.extend(extra.iter().cloned());
        }
        oracle_obj.insert(key.to_owned(), Value::Array(combined));
    }
    Ok(())
}

/// Render the declarative config into a seeded genesis and write `genesis.seeded.json` and `validators.json` into `work_dir`.
pub fn run_seed_stage(
    work_dir: &Path,
    inputs: &SeedInputs,
    config: &Value,
) -> Result<PathBuf, GenesisError> {
    let worldwide_day = resolve_worldwide_day(config, &inputs.base_genesis)?;
    let contracts_dir = config
        .get("contracts_dir")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_contracts_dir().display().to_string());
    let canon_dir = config
        .get("canon_dir")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_canon_dir().display().to_string());
    let fresh_metadosis = config
        .get("fresh_metadosis")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut seeded = inputs.base_genesis.clone();
    let options = SeedOptions {
        contracts_dir: Some(contracts_dir),
        canon_dir: Some(canon_dir),
        worldwide_day,
        fresh_metadosis,
    };
    apply_seed(
        &mut seeded,
        &inputs.seed,
        Some(&inputs.validators),
        Some(&options),
    )?;

    let seeded_path = work_dir.join("genesis.seeded.json");
    let seeded_json = serde_json::to_string_pretty(&seeded)?;
    fs::write(&seeded_path, seeded_json).map_err(|err| GenesisError::io(&seeded_path, err))?;

    let validators_path = work_dir.join("validators.json");
    let validators_json = serde_json::to_string_pretty(&inputs.validators)?;
    fs::write(&validators_path, validators_json)
        .map_err(|err| GenesisError::io(&validators_path, err))?;

    Ok(seeded_path)
}

fn resolve_worldwide_day(
    config: &Value,
    base_genesis: &Value,
) -> Result<Option<u32>, GenesisError> {
    if let Some(raw_day) = config.as_object().and_then(|obj| obj.get("worldwide_day")) {
        if raw_day.is_null() {
            return Ok(None);
        }
        let day = parse_u64(raw_day)?;
        return u32::try_from(day)
            .map(Some)
            .map_err(|_| GenesisError::validation("worldwide_day out of u32 range"));
    }
    if config
        .get("fresh_metadosis")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let Some(ts_str) = base_genesis.get("timestamp").and_then(Value::as_str) else {
        return Err(GenesisError::validation(
            "base_genesis missing hex timestamp",
        ));
    };
    let clean = ts_str
        .strip_prefix("0x")
        .or_else(|| ts_str.strip_prefix("0X"))
        .unwrap_or(ts_str);
    let timestamp = u64::from_str_radix(clean, 16)
        .map_err(|_| GenesisError::validation(format!("invalid timestamp: {ts_str}")))?;
    Ok(Some(timestamp_to_utc_date_key(timestamp)))
}

/// Seed one genesis from an explicit profile without running the TEE stage.
pub fn seed_genesis_from_config(
    inputs: &SeedInputs,
    options: &SeedOptions,
) -> Result<Value, GenesisError> {
    let mut settings = Map::new();
    if let Some(contracts_dir) = &options.contracts_dir {
        settings.insert(
            "contracts_dir".to_owned(),
            Value::String(contracts_dir.clone()),
        );
    }
    match options.worldwide_day {
        Some(day) => {
            settings.insert("worldwide_day".to_owned(), Value::from(u64::from(day)));
        }
        None => {
            settings.insert("worldwide_day".to_owned(), Value::Null);
        }
    }
    if let Some(canon_dir) = &options.canon_dir {
        settings.insert("canon_dir".to_owned(), Value::String(canon_dir.clone()));
    }
    if options.fresh_metadosis {
        settings.insert("fresh_metadosis".to_owned(), Value::Bool(true));
    }

    let temp_dir = tempfile::Builder::new()
        .prefix("outbe-seed-")
        .tempdir()
        .map_err(|err| GenesisError::io(std::env::temp_dir(), err))?;
    let seeded_path = run_seed_stage(temp_dir.path(), inputs, &Value::Object(settings))?;
    let content =
        fs::read_to_string(&seeded_path).map_err(|err| GenesisError::io(&seeded_path, err))?;
    Ok(serde_json::from_str(&content)?)
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_utc_iso8601(timestamp: u64) -> Result<String, GenesisError> {
    let dt = OffsetDateTime::from_unix_timestamp(timestamp as i64)
        .map_err(|e| GenesisError::validation(format!("invalid timestamp {timestamp}: {e}")))?;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    ))
}
