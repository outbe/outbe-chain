//! Genesis configuration construction, base-genesis validation, and Hyperlane admin checks.

use std::{
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use alloy_primitives::Address;
use eyre::{ensure, Context, Result};
use outbe_genesis_cli::seed_codec::{
    HYPERLANE_CONTROLLER_ADDRESS, HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT,
};
use outbe_operator_node_role::{validator_address, FEEDER_FUNDING_WEI};
use outbe_primitives::tee_attestation_v1::{NetworkBindingV1, TeePolicyScheduleV1};
use serde_json::{json, Map, Value};

use super::build_joining::feeder_pairs_path;
use crate::network::config::{
    Founder, Network, NetworkConfig, BASE_CONSENSUS_P2P_PORT, BASE_RPC_PORT,
    DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS, DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
    DEFAULT_EPOCH_LENGTH_BLOCKS,
};

pub(super) fn stage_base_genesis(
    config_path: &Path,
    config: &NetworkConfig,
    staging: &Path,
) -> Result<Option<PathBuf>> {
    let Some(configured) = &config.genesis.base else {
        return Ok(None);
    };
    let source = if configured.is_absolute() {
        configured.clone()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(configured)
    };
    ensure!(
        source.is_file(),
        "genesis.base does not exist: {}",
        source.display()
    );
    let genesis: Value = serde_json::from_slice(&fs::read(&source)?)?;
    ensure_base_genesis(&genesis, config.chain_id)?;
    let destination = staging.join("genesis.base.json");
    fs::copy(source, &destination)?;
    Ok(Some(destination))
}

pub(super) fn ensure_base_genesis(genesis: &Value, chain_id: u64) -> Result<()> {
    ensure!(
        genesis.is_object(),
        "genesis.base must contain a JSON object"
    );
    ensure!(
        genesis.pointer("/config/chainId").and_then(Value::as_u64) == Some(chain_id),
        "genesis.base config.chainId must match network chain_id {chain_id}"
    );
    ensure!(
        genesis.pointer("/config/teeAttestationV1").is_none(),
        "genesis.base must be a pre-TEE genesis without config.teeAttestationV1"
    );
    ensure!(
        genesis.get("alloc").is_some_and(Value::is_object),
        "genesis.base alloc must be a JSON object"
    );
    Ok(())
}

pub(crate) fn genesis_identity(path: &Path) -> Result<(String, NetworkBindingV1)> {
    let genesis: Value = serde_json::from_slice(&fs::read(path)?)?;
    let encoded = genesis
        .pointer("/config/teeAttestationV1/policySchedule")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("final genesis has no TEE policy schedule"))?;
    let schedule = TeePolicyScheduleV1::decode_canonical(&hex::decode(
        encoded.strip_prefix("0x").unwrap_or(encoded),
    )?)
    .wrap_err("invalid final TEE policy schedule")?;
    let policy = schedule
        .active_policy(1)
        .wrap_err("invalid final TEE policy")?;
    let binding = NetworkBindingV1 {
        chain_id: schedule.chain_id,
        genesis_hash: schedule.genesis_hash,
        attestation_mode: policy.attestation_mode,
    };
    Ok((format!("{:#x}", schedule.genesis_hash), binding))
}

pub(crate) fn genesis_cli_config(
    config: &NetworkConfig,
    software: &Path,
    keys: &Path,
    base_genesis: Option<&Path>,
) -> Result<Value> {
    let mut root = config
        .genesis
        .overrides
        .iter()
        .map(|(key, value)| Ok((key.clone(), serde_json::to_value(value)?)))
        .collect::<Result<Map<String, Value>>>()?;
    insert_core_genesis_fields(&mut root, config, software, keys)?;
    if config.vm.is_none() {
        apply_bare_host_rpc_ports(&mut root, config);
    }
    if config.network == Network::Devnet {
        apply_devnet_hyperlane_admin(&mut root, keys)?;
    }
    apply_genesis_schedule(&mut root, config, base_genesis);
    apply_genesis_balances(&mut root, config, keys)?;
    Ok(Value::Object(root))
}

fn genesis_validator_entries(config: &NetworkConfig) -> Result<Vec<Value>> {
    config
        .founders
        .iter()
        .map(|founder| genesis_validator_entry(config, founder))
        .collect()
}

fn genesis_validator_entry(config: &NetworkConfig, founder: &Founder) -> Result<Value> {
    let port_offset = if config.vm.is_some() {
        config.resolve_founder_vm(founder)?.port_offset
    } else {
        0
    };
    if port_offset == 0 {
        return Ok(Value::String(founder.advertise_ip.to_string()));
    }
    let consensus_port = BASE_CONSENSUS_P2P_PORT
        .checked_add(port_offset)
        .ok_or_else(|| {
            eyre::eyre!(
                "port_offset {port_offset} overflows consensus port for {}",
                founder.name
            )
        })?;
    Ok(Value::String(format!(
        "{}:{consensus_port}",
        founder.advertise_ip
    )))
}

fn insert_core_genesis_fields(
    root: &mut Map<String, Value>,
    config: &NetworkConfig,
    software: &Path,
    keys: &Path,
) -> Result<()> {
    let validators = genesis_validator_entries(config)?;
    let hyperlane_origins = config
        .resolve_hyperlane_config(&config.founders[0])?
        .origins;
    // `outbe-genesis-cli` maps known chain IDs (424242 -> devnet, 54322345 -> testnet, 676 ->
    // mainnet) strictly. When a `testnet`-profile network selects the devnet chain ID (424242),
    // pass `"devnet"` only as the genesis-cli identity label while keeping `config.network` as
    // `Testnet` for feeder/profile/manifest behavior (chain code stays untouched).
    let genesis_network_label =
        if config.network == Network::Testnet && config.chain_id == Network::Devnet.chain_id() {
            "devnet"
        } else {
            config.network.as_str()
        };
    let public_urls: Vec<_> = config
        .founders
        .iter()
        .map(|founder| &founder.hyperlane_public_url)
        .collect();
    for (key, value) in [
        ("network", json!(genesis_network_label)),
        ("chain_id", json!(config.chain_id)),
        ("validators", Value::Array(validators)),
        ("keys_dir", json!(keys)),
        (
            "base_profile",
            json!(software.join("share/outbe-genesis/profiles/testnet.yaml")),
        ),
        (
            "contracts_dir",
            json!(software.join("share/outbe-genesis/contracts/predeploys")),
        ),
        ("feeder_pairs", json!(feeder_pairs_path(config, software))),
        ("remote_base_dir", json!("/opt/outbe-chain")),
        ("remote_keys_dir", json!("/opt/outbe-chain/keys")),
        ("node_binary", json!("/opt/outbe-chain/outbe-chain")),
        ("radicle_binary", json!("/opt/outbe-chain/outbe-radicle")),
        ("feeder_binary", json!("/opt/outbe-chain/outbe-feeder")),
        (
            "hyperlane_binary",
            json!("/opt/outbe-chain/outbe-hyperlane-validator"),
        ),
        (
            "hyperlane_poll_interval_secs",
            json!(config.hyperlane.poll_interval_secs),
        ),
        ("hyperlane_gasless", json!(config.hyperlane.gasless)),
        (
            "hyperlane_signer_funding_wei",
            json!(config.hyperlane.signer_funding_wei),
        ),
        ("hyperlane_public_urls", json!(public_urls)),
        ("hyperlane_origins", json!(hyperlane_origins)),
        ("crs_path", json!(software.join(".bb-crs/bn254_g1.dat"))),
        (
            "consensus_use_local_defaults",
            json!(config.topology.use_local_defaults),
        ),
        ("rpc_bind_ip", json!(rpc_bind(config)?)),
        ("tee", json!({"mode": "no-attestation-dev"})),
    ] {
        root.insert(key.to_owned(), value);
    }
    Ok(())
}

fn apply_bare_host_rpc_ports(root: &mut Map<String, Value>, config: &NetworkConfig) {
    let first_rpc_port = config.founders[0].rpc_port();
    let uniform = config
        .founders
        .iter()
        .all(|founder| founder.rpc_port() == first_rpc_port);
    if uniform {
        if first_rpc_port != BASE_RPC_PORT {
            root.insert("rpc_port".into(), json!(first_rpc_port));
        }
        return;
    }
    let runtime_entries: Vec<Value> = config
        .founders
        .iter()
        .map(|founder| json!({"rpc_port": founder.rpc_port()}))
        .collect();
    root.insert("validator_runtime".into(), Value::Array(runtime_entries));
}

fn apply_devnet_hyperlane_admin(root: &mut Map<String, Value>, keys: &Path) -> Result<()> {
    root.insert("price_provider".into(), json!("mock"));
    let founder0 = validator_address(&keys.join("validator-0/evm-key.hex"))?;
    let founder0_hex = format!("{founder0:#x}");
    let hc = root
        .entry("hyperlane_controller")
        .or_insert_with(|| Value::Object(Map::new()));
    let hc_obj = hc
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("genesis.overrides.hyperlane_controller must be a mapping"))?;
    if let Some(existing) = hc_obj.get("admin") {
        let existing_str = existing.as_str().ok_or_else(|| {
            eyre::eyre!("genesis.overrides.hyperlane_controller.admin must be a hex address string")
        })?;
        let parsed: Address = existing_str.parse().wrap_err_with(|| {
            format!("invalid genesis.overrides.hyperlane_controller.admin: {existing_str}")
        })?;
        ensure!(
            parsed == founder0,
            "devnet sets HyperlaneController bootstrap admin to founder 0 ({founder0_hex}); \
             remove conflicting genesis.overrides.hyperlane_controller.admin ({existing_str})"
        );
    }
    hc_obj.insert("admin".into(), json!(founder0_hex));
    Ok(())
}

fn apply_genesis_schedule(
    root: &mut Map<String, Value>,
    config: &NetworkConfig,
    base_genesis: Option<&Path>,
) {
    if let Some(path) = base_genesis {
        root.insert("base_genesis".into(), json!(path));
    } else {
        root.insert(
            "epoch_length_blocks".into(),
            json!(config
                .genesis
                .epoch_length_blocks
                .unwrap_or(DEFAULT_EPOCH_LENGTH_BLOCKS)),
        );
        root.insert(
            "dkg_prepare_window_blocks".into(),
            json!(config
                .genesis
                .dkg_prepare_window_blocks
                .unwrap_or(DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)),
        );
        root.insert(
            "dkg_activation_grace_blocks".into(),
            json!(config
                .genesis
                .dkg_activation_grace_blocks
                .unwrap_or(DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)),
        );
    }
    if let Some(timestamp) = config.genesis.timestamp {
        root.insert("timestamp".into(), json!(timestamp));
    }
}

fn apply_genesis_balances(
    root: &mut Map<String, Value>,
    config: &NetworkConfig,
    keys: &Path,
) -> Result<()> {
    let balance = root
        .entry("balance")
        .or_insert_with(|| Value::Object(Map::new()));
    let balance = balance
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("genesis.overrides.balance must be a mapping"))?;
    for (address, amount) in &config.genesis.prefunded_accounts {
        ensure!(
            balance.insert(address.clone(), json!(amount)).is_none(),
            "prefunded account is also present in genesis.overrides.balance: {address}"
        );
    }
    for index in 0..config.founders.len() {
        let feeder = validator_address(&keys.join(format!("validator-{index}/feeder-key.hex")))?;
        let address = format!("{feeder:#x}");
        ensure!(
            balance
                .insert(address.clone(), json!(FEEDER_FUNDING_WEI.to_string()))
                .is_none(),
            "feeder signer is already present in genesis balance: {address}"
        );
    }
    Ok(())
}

fn rpc_bind(config: &NetworkConfig) -> Result<String> {
    let binds: Vec<Ipv4Addr> = config
        .founders
        .iter()
        .map(|founder| config.founder_rpc_bind_ip(founder))
        .collect::<Result<Vec<_>>>()?;
    if config
        .founders
        .iter()
        .zip(&binds)
        .all(|(founder, bind)| *bind == founder.advertise_ip)
    {
        return Ok("validator".into());
    }
    let first = binds[0];
    if binds.iter().all(|bind| *bind == first) {
        return Ok(first.to_string());
    }
    // For VM networks where each VM binds its own private VM IP while advertising a shared
    // host public IP, `launch_bundle::render` is not used — each VM receives its exact
    // `rpc_bind_ip` (defaulting to `vm.vm_ip`) in `emit_vm_founder_trees`.
    if config.vm.is_some() {
        return Ok("127.0.0.1".into());
    }
    eyre::bail!("founder rpc_bind_ip values must all match or equal each founder advertise_ip");
}

pub(crate) fn verify_devnet_hyperlane_admin(
    genesis_path: &Path,
    expected_admin: Address,
) -> Result<()> {
    let genesis: Value =
        serde_json::from_slice(&fs::read(genesis_path).wrap_err_with(|| {
            format!("read genesis for admin check: {}", genesis_path.display())
        })?)?;
    let actual = read_hyperlane_admin_slot(&genesis)?;
    ensure!(
        actual == expected_admin,
        "devnet HyperlaneController slot {HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT} admin \
         mismatch: expected founder 0 ({expected_admin:#x}), got {actual:#x}"
    );
    Ok(())
}

pub(super) fn read_hyperlane_admin_slot(genesis: &Value) -> Result<Address> {
    let alloc = genesis
        .get("alloc")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre::eyre!("genesis is missing alloc object"))?;
    let target_addr = HYPERLANE_CONTROLLER_ADDRESS
        .strip_prefix("0x")
        .unwrap_or(HYPERLANE_CONTROLLER_ADDRESS)
        .to_ascii_lowercase();
    let entry = alloc
        .iter()
        .find(|(addr, _)| {
            addr.strip_prefix("0x")
                .unwrap_or(addr)
                .eq_ignore_ascii_case(&target_addr)
        })
        .map(|(_, val)| val)
        .ok_or_else(|| {
            eyre::eyre!(
                "genesis alloc is missing HyperlaneController ({HYPERLANE_CONTROLLER_ADDRESS})"
            )
        })?;
    let storage = entry
        .get("storage")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre::eyre!("HyperlaneController alloc entry has no storage"))?;
    let slot_hex = format!("{HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT:064x}");
    let raw_word = storage
        .iter()
        .find(|(slot, _)| {
            slot.strip_prefix("0x")
                .unwrap_or(slot)
                .eq_ignore_ascii_case(&slot_hex)
        })
        .and_then(|(_, val)| val.as_str())
        .ok_or_else(|| {
            eyre::eyre!(
                "HyperlaneController storage is missing bootstrap admin slot {HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT}"
            )
        })?;
    let word_hex = raw_word.strip_prefix("0x").unwrap_or(raw_word);
    let word_bytes = hex::decode(word_hex).wrap_err("invalid hex in HyperlaneController slot 9")?;
    ensure!(
        word_bytes.len() == 32,
        "expected 32-byte word in HyperlaneController slot 9, got {} bytes",
        word_bytes.len()
    );
    Ok(Address::from_slice(&word_bytes[12..32]))
}
