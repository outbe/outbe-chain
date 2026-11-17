//! Build or load the pre-TEE Reth genesis structure.

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::{
    error::GenesisError,
    network::{
        DEFAULT_CHAIN_ID, DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS, DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
        DEFAULT_EPOCH_LENGTH_BLOCKS, DEFAULT_GAS_LIMIT, MAX_GENESIS_AGE_SECONDS,
    },
    seed_codec::parse_u64,
};

/// Build the base Reth genesis structure from `config`.
pub fn build_base_genesis(config: &Value) -> Result<Value, GenesisError> {
    if let Some(path) = config.get("base_genesis").and_then(Value::as_str) {
        return load_base_genesis(Path::new(path), config);
    }
    let now = current_unix_timestamp();
    let timestamp = match config.get("timestamp") {
        Some(value) => parse_u64(value)?,
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
        Some(Value::String(value)) => value.clone(),
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

fn load_base_genesis(path: &Path, config: &Value) -> Result<Value, GenesisError> {
    for key in [
        "timestamp",
        "epoch_length_blocks",
        "dkg_prepare_window_blocks",
        "dkg_activation_grace_blocks",
        "gas_limit",
    ] {
        if config.get(key).is_some() {
            return Err(GenesisError::validation(format!(
                "`{key}` cannot be combined with `base_genesis`; define it in the base genesis"
            )));
        }
    }
    let raw = fs::read_to_string(path).map_err(|error| GenesisError::io(path, error))?;
    let genesis: Value = serde_json::from_str(&raw)?;
    let expected_chain_id = u64_or_default(config, "chain_id", DEFAULT_CHAIN_ID)?;
    let chain_id = genesis
        .pointer("/config/chainId")
        .and_then(Value::as_u64)
        .ok_or_else(|| GenesisError::validation("base_genesis missing numeric config.chainId"))?;
    if chain_id != expected_chain_id {
        return Err(GenesisError::validation(format!(
            "base_genesis chain id {chain_id} does not match configured chain id {expected_chain_id}"
        )));
    }
    if genesis.pointer("/config/teeAttestationV1").is_some() {
        return Err(GenesisError::validation(
            "base_genesis must not contain config.teeAttestationV1",
        ));
    }
    if genesis.get("alloc").is_none_or(|value| !value.is_object()) {
        return Err(GenesisError::validation(
            "base_genesis alloc must be a JSON object",
        ));
    }
    Ok(genesis)
}

fn u64_or_default(config: &Value, key: &str, default: u64) -> Result<u64, GenesisError> {
    match config.get(key) {
        Some(value) => parse_u64(value),
        None => Ok(default),
    }
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn format_utc_iso8601(timestamp: u64) -> Result<String, GenesisError> {
    let datetime = OffsetDateTime::from_unix_timestamp(timestamp as i64).map_err(|error| {
        GenesisError::validation(format!("invalid timestamp {timestamp}: {error}"))
    })?;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        datetime.year(),
        u8::from(datetime.month()),
        datetime.day(),
        datetime.hour(),
        datetime.minute(),
        datetime.second()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_matching_pre_tee_base_genesis() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genesis.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "config": {"chainId": 54_322_345},
                "alloc": {},
                "timestamp": "0x1"
            }))
            .unwrap(),
        )
        .unwrap();
        let config = json!({
            "chain_id": 54_322_345,
            "base_genesis": path
        });
        assert_eq!(
            build_base_genesis(&config).unwrap()["config"]["chainId"],
            54_322_345
        );
    }
}
