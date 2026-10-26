//! Network identities, defaults and configuration validation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::GenesisError;
use crate::launch_bundle::DEFAULT_PORTS;
use crate::seed_codec::parse_i128;

pub const DEFAULT_CHAIN_ID: u64 = 424242;
pub const DEVNET_CHAIN_ID: u64 = 424242;
pub const TESTNET_CHAIN_ID: u64 = 54322345;
pub const MAINNET_CHAIN_ID: u64 = 676;

pub const NETWORK_IDENTITIES: [(&str, u64, &str); 3] = [
    ("devnet", DEVNET_CHAIN_ID, "outbe-devnet-1"),
    ("testnet", TESTNET_CHAIN_ID, "outbe-testnet-1"),
    ("mainnet", MAINNET_CHAIN_ID, "outbe-mainnet-1"),
];

pub const DEFAULT_GAS_LIMIT: &str = "0x1c9c380";
pub const DEFAULT_EPOCH_LENGTH_BLOCKS: u64 = 300;
pub const DEFAULT_DKG_PREPARE_WINDOW_BLOCKS: u64 = 30;
pub const DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS: u64 = 30;
pub const DEFAULT_CONSENSUS_P2P_PORT: u16 = 30400;
pub const DEFAULT_PREFUND_COEN_UNITS: u128 = 10_000 * 1_000_000_000_000_000_000;
pub const FOUNDER_COUNT: usize = 4;
pub const MAX_GENESIS_AGE_SECONDS: i64 = 6 * 60 * 60;

pub const SEED_SECTIONS: [&str; 15] = [
    "protocol_constants",
    "balance",
    "staking",
    "rewards",
    "validator_set",
    "code_registry",
    "metadosis",
    "oracle",
    "tributes",
    "intex_profile",
    "gem_profile",
    "nod_profile",
    "credis_profile",
    "contracts",
    "tee_policy",
];

pub const TEE_KEYS: [&str; 6] = [
    "mode",
    "mrenclave",
    "mrsigner",
    "isv_prod_id",
    "minimum_isv_svn",
    "minimum_tcb_evaluation_data_number",
];

const STATIC_TOP_LEVEL_KEYS: [&str; 38] = [
    "network",
    "validators",
    "keys_dir",
    "chain_id",
    "tee",
    "output",
    "chain_binary",
    "keygen_binary",
    "consensus_p2p_port",
    "gas_limit",
    "timestamp",
    "epoch_length_blocks",
    "dkg_prepare_window_blocks",
    "dkg_activation_grace_blocks",
    "prefund_coen_units",
    "worldwide_day",
    "fresh_metadosis",
    "contracts_dir",
    "canon_dir",
    "enclave_image",
    "enclave_dir",
    "enclave_runner",
    "enclave_sgx",
    "signed_enclave_dir",
    "allow_stale_timestamp",
    "node_binary",
    "radicle_binary",
    "radicle_external_inbound_reserve",
    "feeder_binary",
    "remote_base_dir",
    "remote_keys_dir",
    "price_provider",
    "public_rpc_port",
    "public_radicle_status_port",
    "price_feed_rest",
    "price_feed_websocket",
    "feeder_pairs",
    "feeder_seeds",
];

/// Return the repository root directory (`<repo>`).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("bin/outbe-genesis-cli lives two levels below workspace root")
        .to_path_buf()
}

/// Return the `profiles` directory (`<repo>/bin/outbe-genesis-cli/profiles`).
pub fn profiles_dir() -> PathBuf {
    repo_root().join("bin/outbe-genesis-cli/profiles")
}

/// Return the default `canon` directory (`<repo>/bin/outbe-genesis-cli/canon`).
pub fn default_canon_dir() -> PathBuf {
    repo_root().join("bin/outbe-genesis-cli/canon")
}

/// Return the default `contracts` predeploy directory (`<repo>/contracts/predeploys`).
pub fn default_contracts_dir() -> PathBuf {
    repo_root().join("contracts/predeploys")
}

/// Return the baseline profile path (`<repo>/bin/outbe-genesis-cli/profiles/testnet.yaml`).
pub fn base_profile_path() -> PathBuf {
    profiles_dir().join("testnet.yaml")
}

/// Return the complete set of allowed top-level YAML configuration keys.
pub fn top_level_keys() -> BTreeSet<&'static str> {
    STATIC_TOP_LEVEL_KEYS
        .into_iter()
        .chain(DEFAULT_PORTS.iter().map(|&(port_key, _)| port_key))
        .chain(SEED_SECTIONS)
        .collect()
}

/// Scalars and lists replace; mappings merge key by key.
pub fn deep_merge(base: Option<&Value>, override_val: &Value) -> Value {
    if let (Some(Value::Object(base_map)), Value::Object(override_map)) = (base, override_val) {
        let mut merged: Map<String, Value> = base_map.clone();
        for (key, value) in override_map {
            let next = deep_merge(base_map.get(key), value);
            merged.insert(key.clone(), next);
        }
        return Value::Object(merged);
    }
    override_val.clone()
}

/// Validate a network YAML configuration object.
pub fn validate_config(config: &Value) -> Result<(), GenesisError> {
    let Some(obj) = config.as_object() else {
        return Err(GenesisError::validation(
            "network configuration must be a mapping",
        ));
    };
    if obj.contains_key("intex_factory") {
        return Err(GenesisError::validation(
            "intex_factory was renamed to intex_profile",
        ));
    }
    let allowed = top_level_keys();
    let mut unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        let allowed_list: Vec<&str> = allowed.into_iter().collect();
        return Err(GenesisError::validation(format!(
            "unknown key(s): {}; allowed: {}",
            unknown.join(", "),
            allowed_list.join(", ")
        )));
    }
    validate_founders(obj)?;
    validate_tee(config, obj)?;
    validate_ports(obj)
}

fn validate_founders(obj: &Map<String, Value>) -> Result<(), GenesisError> {
    let hosts = obj
        .get("validators")
        .and_then(Value::as_array)
        .filter(|h| h.len() == FOUNDER_COUNT)
        .ok_or_else(|| {
            GenesisError::validation(format!(
                "`validators` must list exactly {FOUNDER_COUNT} hosts (the bootstrap network profile uses four founders)"
            ))
        })?;
    if let Some((index, _)) = hosts
        .iter()
        .enumerate()
        .find(|(_, h)| h.as_str().is_none_or(|s| s.trim().is_empty()))
    {
        return Err(GenesisError::validation(format!(
            "validators[{index}] must be a host or IP string"
        )));
    }
    if !obj.contains_key("keys_dir") {
        return Err(GenesisError::validation(
            "`keys_dir` is required: it holds validator-N/ key material",
        ));
    }
    Ok(())
}

fn validate_tee(config: &Value, obj: &Map<String, Value>) -> Result<(), GenesisError> {
    let Some(Value::Object(tee)) = obj.get("tee") else {
        return Err(GenesisError::validation(
            "`tee` section with `mode` is required",
        ));
    };
    let allowed_tee: BTreeSet<&str> = TEE_KEYS.into_iter().collect();
    let mut unknown_tee: Vec<&str> = tee
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed_tee.contains(k))
        .collect();
    unknown_tee.sort_unstable();
    if !unknown_tee.is_empty() {
        return Err(GenesisError::validation(format!(
            "unknown tee key(s): {}",
            unknown_tee.join(", ")
        )));
    }
    let (network, chain_id, _) = network_identity(config)?;
    let mode = tee.get("mode").and_then(Value::as_str).unwrap_or("");
    if network == "mainnet" {
        validate_mainnet(obj, mode)?;
    }
    validate_tee_mode(obj, mode, chain_id)
}

fn validate_tee_mode(
    obj: &Map<String, Value>,
    mode: &str,
    chain_id: u64,
) -> Result<(), GenesisError> {
    match mode {
        "gramine-direct-dev" => {
            if chain_id != DEVNET_CHAIN_ID && chain_id != TESTNET_CHAIN_ID {
                return Err(GenesisError::validation(format!(
                    "tee.mode gramine-direct-dev requires the devnet or testnet chain id ({DEVNET_CHAIN_ID} or {TESTNET_CHAIN_ID}), not {chain_id}"
                )));
            }
            Ok(())
        }
        "dcap-required" => validate_dcap_mode(obj, chain_id),
        _ => Err(GenesisError::validation(
            "tee.mode must be gramine-direct-dev or dcap-required",
        )),
    }
}

fn validate_dcap_mode(obj: &Map<String, Value>, chain_id: u64) -> Result<(), GenesisError> {
    if !matches!(
        chain_id,
        DEVNET_CHAIN_ID | TESTNET_CHAIN_ID | MAINNET_CHAIN_ID
    ) {
        return Err(GenesisError::validation(format!(
            "tee.mode dcap-required requires a canonical Outbe chain id, not {chain_id}"
        )));
    }
    let image = obj
        .get("enclave_image")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !is_pinned_sha256_image(image) {
        return Err(GenesisError::validation(
            "tee.mode dcap-required needs `enclave_image` pinned to an immutable digest (name@sha256:<64 lowercase hex>), not a mutable tag",
        ));
    }
    Ok(())
}

fn is_pinned_sha256_image(image: &str) -> bool {
    let Some((name, digest)) = image.split_once("@sha256:") else {
        return false;
    };
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_mainnet(obj: &Map<String, Value>, mode: &str) -> Result<(), GenesisError> {
    if mode != "dcap-required" {
        return Err(GenesisError::validation(
            "Mainnet requires tee.mode dcap-required",
        ));
    }
    if obj.contains_key("protocol_constants") {
        return Err(GenesisError::validation(
            "Mainnet forbids protocol_constants overrides and uses canonical production defaults",
        ));
    }
    let endpoint = obj
        .get("price_feed_rest")
        .and_then(Value::as_str)
        .unwrap_or("");
    if endpoint.is_empty() {
        return Err(GenesisError::validation(
            "Mainnet requires an explicit production price feed REST endpoint",
        ));
    }
    if endpoint.to_ascii_lowercase().contains("testnet") {
        return Err(GenesisError::validation(
            "Mainnet may not use a testnet price endpoint",
        ));
    }
    Ok(())
}

fn validate_ports(obj: &Map<String, Value>) -> Result<(), GenesisError> {
    let mut ports: BTreeMap<&str, i128> = BTreeMap::new();
    for &(name, default_port) in DEFAULT_PORTS {
        let port_value = obj
            .get(name)
            .map(parse_i128)
            .transpose()?
            .unwrap_or_else(|| i128::from(default_port));
        if port_value != 0 {
            ports.insert(name, port_value);
        }
    }
    let consensus_port = obj
        .get("consensus_p2p_port")
        .map(parse_i128)
        .transpose()?
        .unwrap_or_else(|| i128::from(DEFAULT_CONSENSUS_P2P_PORT));
    ports.insert("consensus_p2p_port", consensus_port);

    let mut seen: BTreeMap<i128, &str> = BTreeMap::new();
    for (name, port_value) in ports {
        if !(1..=65535).contains(&port_value) {
            return Err(GenesisError::validation(format!(
                "{name} is outside 1..65535: {port_value}"
            )));
        }
        if let Some(prev) = seen.insert(port_value, name) {
            return Err(GenesisError::validation(format!(
                "port collision: {prev} and {name} both use {port_value}"
            )));
        }
    }
    Ok(())
}

/// Resolve and validate the `(network, chain_id, chain_name)` tuple from `config`.
pub fn network_identity(config: &Value) -> Result<(String, u64, String), GenesisError> {
    let chain_id_raw = config
        .get("chain_id")
        .map(parse_i128)
        .transpose()?
        .unwrap_or_else(|| i128::from(DEFAULT_CHAIN_ID));
    let chain_id = u64::try_from(chain_id_raw)
        .map_err(|_| GenesisError::validation(format!("unknown Outbe chain id {chain_id_raw}")))?;
    let inferred = NETWORK_IDENTITIES
        .iter()
        .find_map(|&(name, id, _)| (id == chain_id).then_some(name))
        .ok_or_else(|| GenesisError::validation(format!("unknown Outbe chain id {chain_id}")))?;
    let requested = resolve_requested_network(config, inferred)?;
    let &(_, expected_id, chain_name) = NETWORK_IDENTITIES
        .iter()
        .find(|&&(name, _, _)| name == requested)
        .ok_or_else(|| GenesisError::validation(format!("unknown Outbe network '{requested}'")))?;
    if chain_id != expected_id {
        return Err(GenesisError::validation(format!(
            "{requested} requires chain id {expected_id}, not {chain_id}"
        )));
    }
    Ok((requested.to_owned(), chain_id, chain_name.to_owned()))
}

fn resolve_requested_network<'a>(
    config: &'a Value,
    inferred: &'a str,
) -> Result<&'a str, GenesisError> {
    match config.get("network").and_then(Value::as_str) {
        Some(req) => Ok(req),
        None if inferred == "mainnet" => Err(GenesisError::validation(
            "Mainnet chain id 676 requires explicit `network: mainnet`",
        )),
        None => Ok(inferred),
    }
}

/// Resolve a host binary (`outbe-chain` or `outbe-keygen`) from `config[key]` or `target/{release,debug}`.
pub fn resolve_binary(config: &Value, key: &str, name: &str) -> Result<String, GenesisError> {
    let root = repo_root();
    let candidates = match config.get(key).and_then(Value::as_str) {
        Some(explicit) => [PathBuf::from(explicit), root.join(explicit)],
        None => [
            root.join("target").join("release").join(name),
            root.join("target").join("debug").join(name),
        ],
    };
    candidates
        .into_iter()
        .find(|c| c.is_file())
        .map(|c| c.to_string_lossy().into_owned())
        .ok_or_else(|| {
            GenesisError::validation(format!(
                "{name} binary not found; build it or set `{key}` in the yaml"
            ))
        })
}
