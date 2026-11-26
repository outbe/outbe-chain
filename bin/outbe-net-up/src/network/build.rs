use std::{
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use alloy_primitives::Address;
use eyre::{ensure, Context, Result};
use outbe_genesis_cli::{
    attestation::run_tee_stage,
    create_genesis::create_seeded_genesis,
    launch_bundle,
    seed_codec::{HYPERLANE_CONTROLLER_ADDRESS, HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT},
};
use outbe_operator_node_role::{
    bundle::{self, BundleInput},
    config::{
        FeederConfig as OperatorFeederConfig, NetworkConfig as OperatorNetworkConfig,
        NetworkPorts as OperatorNetworkPorts, OperatorConfig,
        CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
    process,
    progress::Progress,
    validator_address, FEEDER_FUNDING_WEI,
};
use outbe_primitives::tee_attestation_v1::{NetworkBindingV1, TeePolicyScheduleV1};
use outbe_software_bundle::{sha256_file, verify_and_extract, SoftwareManifest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use walkdir::WalkDir;

use super::config::{Network, NetworkConfig, VmFlavor};

const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum BuildState {
    Ready,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DeploymentKind {
    #[default]
    Host,
    Vm,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct NetworkManifest {
    pub(super) version: u32,
    pub(super) state: BuildState,
    #[serde(default)]
    pub(crate) deployment: DeploymentKind,
    pub(super) name: String,
    pub(super) network: String,
    pub(super) chain_id: u64,
    pub(super) config_sha256: String,
    pub(super) software: SoftwareIdentity,
    pub(super) genesis: GenesisIdentity,
    pub(crate) founders: Vec<FounderArtifact>,
    pub(super) joining: JoiningArtifact,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct SoftwareIdentity {
    pub(super) archive_sha256: String,
    pub(super) commit: String,
    pub(super) release_tag: String,
    pub(super) target: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct GenesisIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) base_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) base_sha256: Option<String>,
    pub(super) seeded_path: String,
    pub(super) seeded_sha256: String,
    pub(super) final_path: String,
    pub(super) final_sha256: String,
    pub(super) genesis_block_hash: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct FounderArtifact {
    pub(crate) index: usize,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ssh_host: Option<String>,
    pub(super) advertise_ip: Ipv4Addr,
    pub(super) validator_address: String,
    pub(super) hyperlane_address: String,
    pub(super) consensus_public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) archive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vm: Option<FounderVmArtifact>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct FounderVmArtifact {
    pub(crate) data_tree: String,
    pub(super) vmm_host: String,
    pub(super) vmm_user: String,
    pub(super) vmm_port: u16,
    pub(super) bridge_ip: Ipv4Addr,
    #[serde(default)]
    pub(super) port_offset: u16,
    pub(super) vcpu: u32,
    pub(super) memory_gib: u32,
    pub(crate) disk_gib: u32,
    pub(super) flavor: VmFlavor,
    pub(super) has_authorized_keys: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct JoiningArtifact {
    pub(super) bundle: String,
    pub(super) bundle_sha256: String,
    pub(super) config_example: String,
}

pub(super) struct BuildArtifacts<'a> {
    pub(super) root: &'a Path,
    pub(super) software_root: &'a Path,
    pub(super) software: &'a SoftwareManifest,
    pub(super) config: &'a NetworkConfig,
    pub(super) validators: &'a [Value],
}

pub fn run(
    software_bundle: &Path,
    config_path: &Path,
    output: &Path,
    existing_keys: Option<&Path>,
) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("validating network configuration and software bundle");
    ensure!(
        std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64",
        "network build currently executes the bundled Linux binaries and must run on Linux x86_64"
    );
    ensure!(
        !output.exists(),
        "network output already exists: {}",
        output.display()
    );
    let config = NetworkConfig::read(config_path)?;
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let software_root = tempfile::Builder::new()
        .prefix("outbe-software-")
        .tempdir_in(parent)?;
    let software = verify_and_extract(software_bundle, software_root.path())?;
    ensure_supported_software(&software)?;

    progress.stage("generating founding validator identities and genesis");
    let staging = tempfile::Builder::new()
        .prefix(".outbe-network-")
        .tempdir_in(parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    fs::copy(config_path, staging.path().join("network.yaml"))?;
    fs::copy(
        software_bundle,
        staging.path().join("software-bundle.tar.zst"),
    )?;
    let base_genesis = stage_base_genesis(config_path, &config, staging.path())?;

    let keys = staging.path().join("secrets/founder-keys");
    match existing_keys {
        Some(source) => copy_tree(source, &keys)?,
        None => generate_keys(&config, software_root.path(), &keys)?,
    }
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700))?;

    let genesis_config = genesis_cli_config(
        &config,
        software_root.path(),
        &keys,
        base_genesis.as_deref(),
    )?;
    let seeded = staging.path().join("genesis.seeded.json");
    let keygen = software_root.path().join("bin/outbe-keygen");
    let validators = create_seeded_genesis(
        &genesis_config,
        &keys,
        keygen
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
        &seeded,
    )
    .wrap_err("failed to create seeded genesis")?;
    if config.network == Network::Devnet {
        let founder0 = validator_address(&keys.join("validator-0/evm-key.hex"))?;
        verify_devnet_hyperlane_admin(&seeded, founder0)?;
    }

    let final_genesis = staging.path().join("genesis.json");
    run_tee_stage(
        software_root
            .path()
            .join("bin/outbe-chain")
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
        &seeded,
        &final_genesis,
        &genesis_config,
    )
    .wrap_err("failed to finalize the genesis TEE policy")?;
    if config.network == Network::Devnet {
        let founder0 = validator_address(&keys.join("validator-0/evm-key.hex"))?;
        verify_devnet_hyperlane_admin(&final_genesis, founder0)?;
    }
    let deployment = if config.vm.is_some() {
        DeploymentKind::Vm
    } else {
        DeploymentKind::Host
    };
    let founders = match deployment {
        DeploymentKind::Host => {
            launch_bundle::render(&genesis_config, &validators, &final_genesis, &keys)
                .wrap_err("failed to render founder launch bundles")?;
            progress.stage("assembling founder and joining artifacts");
            founder_artifacts(staging.path(), &config, &validators)?
        }
        DeploymentKind::Vm => {
            let bootnodes =
                write_vm_bootnodes(&genesis_config, &config, &validators, &keys, staging.path())?;
            progress.stage("assembling VM founder data trees and joining artifacts");
            emit_vm_founder_trees(&VmTreeInputs {
                staging_root: staging.path(),
                software_root: software_root.path(),
                keys_dir: &keys,
                config_path,
                config: &config,
                validators: &validators,
                bootnodes: &bootnodes,
            })?
        }
    };
    println!("\nHyperlane signers:");
    for founder in &founders {
        println!("  {:<20} {}", founder.name, founder.hyperlane_address);
    }
    let joining = build_joining_artifacts(&BuildArtifacts {
        root: staging.path(),
        software_root: software_root.path(),
        software: &software,
        config: &config,
        validators: &validators,
    })?;
    let genesis_block_hash = genesis_identity(&final_genesis)?.0;
    let manifest = NetworkManifest {
        version: MANIFEST_VERSION,
        state: BuildState::Ready,
        deployment,
        name: config.name.clone(),
        network: config.network.as_str().to_owned(),
        chain_id: config.chain_id,
        config_sha256: sha256_file(config_path)?,
        software: SoftwareIdentity {
            archive_sha256: sha256_file(software_bundle)?,
            commit: software.commit,
            release_tag: software.release_tag,
            target: software.target,
        },
        genesis: GenesisIdentity {
            base_path: base_genesis
                .as_ref()
                .map(|_| "genesis.base.json".to_owned()),
            base_sha256: base_genesis
                .as_ref()
                .map(|path| sha256_file(path))
                .transpose()?,
            seeded_path: "genesis.seeded.json".into(),
            seeded_sha256: sha256_file(&seeded)?,
            final_path: "genesis.json".into(),
            final_sha256: sha256_file(&final_genesis)?,
            genesis_block_hash,
        },
        founders,
        joining,
    };
    fs::write(
        staging.path().join("network-manifest.json"),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    fs::rename(staging.keep(), output)?;
    progress.done(&format!(
        "built ready network artifacts at {}",
        output.display()
    ));
    match deployment {
        DeploymentKind::Host => println!(
            "Next: outbe-net-up network prepare --artifacts {}",
            output.display()
        ),
        DeploymentKind::Vm => println!(
            "Next: outbe-net-up network deploy --artifacts {} --image-archive <path>",
            output.display()
        ),
    }
    Ok(())
}

fn stage_base_genesis(
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

fn ensure_base_genesis(genesis: &Value, chain_id: u64) -> Result<()> {
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

pub(super) fn genesis_identity(path: &Path) -> Result<(String, NetworkBindingV1)> {
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

fn ensure_supported_software(software: &SoftwareManifest) -> Result<()> {
    ensure!(
        software.target == "x86_64-unknown-linux-gnu",
        "unsupported software target: {}",
        software.target
    );
    Ok(())
}

fn generate_keys(config: &NetworkConfig, software: &Path, keys: &Path) -> Result<()> {
    fs::create_dir_all(keys)?;
    let keygen = software.join("bin/outbe-keygen");
    let chain_id = config.chain_id.to_string();
    for index in 0..config.founders.len() {
        let directory = keys.join(format!("validator-{index}"));
        let directory = directory
            .to_str()
            .ok_or_else(|| eyre::eyre!("key path is not UTF-8"))?;
        process::capture(
            &keygen,
            &[
                "validator",
                "--output-dir",
                directory,
                "--chain-id",
                &chain_id,
            ],
        )?;
    }
    Ok(())
}

pub(super) fn genesis_cli_config(
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
    let validators = config
        .founders
        .iter()
        .map(|founder| {
            let port_offset = if config.vm.is_some() {
                config.resolve_founder_vm(founder)?.port_offset
            } else {
                0
            };
            if port_offset == 0 {
                Ok(Value::String(founder.advertise_ip.to_string()))
            } else {
                let consensus_port = super::config::BASE_CONSENSUS_P2P_PORT
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
        })
        .collect::<Result<Vec<_>>>()?;
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
        (
            "hyperlane_public_urls",
            json!(config
                .founders
                .iter()
                .map(|founder| &founder.hyperlane_public_url)
                .collect::<Vec<_>>()),
        ),
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
    if config.network == Network::Devnet {
        root.insert("price_provider".into(), json!("mock"));
        let founder0 = validator_address(&keys.join("validator-0/evm-key.hex"))?;
        let founder0_hex = format!("{founder0:#x}");
        let hc = root
            .entry("hyperlane_controller")
            .or_insert_with(|| Value::Object(Map::new()));
        let hc_obj = hc.as_object_mut().ok_or_else(|| {
            eyre::eyre!("genesis.overrides.hyperlane_controller must be a mapping")
        })?;
        if let Some(existing) = hc_obj.get("admin") {
            let existing_str = existing.as_str().ok_or_else(|| {
                eyre::eyre!(
                    "genesis.overrides.hyperlane_controller.admin must be a hex address string"
                )
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
    }
    if let Some(path) = base_genesis {
        root.insert("base_genesis".into(), json!(path));
    } else {
        root.insert(
            "epoch_length_blocks".into(),
            json!(config
                .genesis
                .epoch_length_blocks
                .unwrap_or(super::config::DEFAULT_EPOCH_LENGTH_BLOCKS)),
        );
        root.insert(
            "dkg_prepare_window_blocks".into(),
            json!(config
                .genesis
                .dkg_prepare_window_blocks
                .unwrap_or(super::config::DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)),
        );
        root.insert(
            "dkg_activation_grace_blocks".into(),
            json!(config
                .genesis
                .dkg_activation_grace_blocks
                .unwrap_or(super::config::DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)),
        );
    }
    if let Some(timestamp) = config.genesis.timestamp {
        root.insert("timestamp".into(), json!(timestamp));
    }
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
    Ok(Value::Object(root))
}

fn rpc_bind(config: &NetworkConfig) -> Result<String> {
    if config
        .founders
        .iter()
        .all(|founder| founder.rpc_bind_ip == founder.advertise_ip)
    {
        return Ok("validator".into());
    }
    let first = config.founders[0].rpc_bind_ip;
    if config
        .founders
        .iter()
        .all(|founder| founder.rpc_bind_ip == first)
    {
        return Ok(first.to_string());
    }
    // For VM networks where each VM binds its own private bridge IP while advertising a shared
    // host public IP, `launch_bundle::render` is not used — each VM receives its exact
    // `founder.rpc_bind_ip` in `emit_vm_founder_trees`.
    if config.vm.is_some() {
        return Ok("127.0.0.1".into());
    }
    eyre::bail!("founder rpc_bind_ip values must all match or equal each founder advertise_ip");
}

pub(super) fn founder_artifacts(
    root: &Path,
    config: &NetworkConfig,
    validators: &[Value],
) -> Result<Vec<FounderArtifact>> {
    config
        .founders
        .iter()
        .zip(validators)
        .enumerate()
        .map(|(index, (founder, validator))| {
            let relative = format!("dist/validator-{index}.tgz");
            let archive = root.join(&relative);
            let ssh = founder.ssh.as_ref().ok_or_else(|| {
                eyre::eyre!("founder {} is missing ssh configuration", founder.name)
            })?;
            Ok(FounderArtifact {
                index,
                name: founder.name.clone(),
                ssh_host: Some(ssh.host.clone()),
                advertise_ip: founder.advertise_ip,
                validator_address: required_validator_field(validator, "address")?.to_owned(),
                hyperlane_address: required_validator_field(validator, "hyperlane_address")?
                    .to_owned(),
                consensus_public_key: required_validator_field(validator, "public_key")?.to_owned(),
                archive: Some(relative),
                archive_sha256: Some(sha256_file(&archive)?),
                vm: None,
            })
        })
        .collect()
}

struct VmTreeInputs<'a> {
    staging_root: &'a Path,
    software_root: &'a Path,
    keys_dir: &'a Path,
    config_path: &'a Path,
    config: &'a NetworkConfig,
    validators: &'a [Value],
    bootnodes: &'a [String],
}

fn write_vm_bootnodes(
    genesis_config: &Value,
    config: &NetworkConfig,
    validators: &[Value],
    keys_dir: &Path,
    staging_root: &Path,
) -> Result<Vec<String>> {
    let reth_p2p_port = launch_bundle::port_of(genesis_config, "reth_p2p_port")?;
    let mut enodes = Vec::with_capacity(validators.len());
    for (index, (founder, validator)) in config.founders.iter().zip(validators).enumerate() {
        let resolved_vm = config.resolve_founder_vm(founder)?;
        let effective_port = reth_p2p_port
            .checked_add(resolved_vm.port_offset)
            .ok_or_else(|| {
                eyre::eyre!(
                    "port_offset {} overflows reth_p2p_port {reth_p2p_port} for {}",
                    resolved_vm.port_offset,
                    founder.name
                )
            })?;
        let node_id =
            launch_bundle::ensure_reth_p2p_secret(&keys_dir.join(format!("validator-{index}")))?;
        let p2p_address = required_validator_field(validator, "p2p_address")?;
        let host = p2p_address
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(p2p_address);
        enodes.push(format!(
            "enode://{node_id}@{}:{effective_port}",
            launch_bundle::format_enode_host(host)
        ));
    }
    fs::write(
        staging_root.join("reth-bootnodes.txt"),
        format!("{}\n", enodes.join("\n")),
    )?;
    Ok(enodes)
}

fn emit_vm_founder_trees(inputs: &VmTreeInputs<'_>) -> Result<Vec<FounderArtifact>> {
    let consensus_peers = consensus_peers(inputs.config, inputs.validators)?;
    let feeder_public_toml = joining_feeder_public_toml(inputs.config, inputs.software_root)?;
    let crs_source = inputs.software_root.join(".bb-crs/bn254_g1.dat");
    ensure!(
        crs_source.is_file(),
        "software bundle is missing CRS: {}",
        crs_source.display()
    );
    let genesis_source = inputs.staging_root.join("genesis.json");
    ensure!(
        genesis_source.is_file(),
        "final genesis is missing: {}",
        genesis_source.display()
    );

    let mut artifacts = Vec::with_capacity(inputs.config.founders.len());
    for (index, (founder, validator)) in inputs
        .config
        .founders
        .iter()
        .zip(inputs.validators)
        .enumerate()
    {
        let resolved_vm = inputs.config.resolve_founder_vm(founder)?;
        let relative = format!("vm/validator-{index}");
        let tree = inputs.staging_root.join(&relative);
        fs::create_dir_all(&tree)?;
        fs::set_permissions(&tree, fs::Permissions::from_mode(0o755))?;

        copy_file_mode(&genesis_source, &tree.join("genesis.json"), 0o644)?;
        copy_file_mode(&crs_source, &tree.join(".bb-crs/bn254_g1.dat"), 0o644)?;

        let src_keys = inputs.keys_dir.join(format!("validator-{index}"));
        let dst_keys = tree.join("keys");
        copy_vm_keys(&src_keys, &dst_keys)?;

        let validator_addr = validator_address(&dst_keys.join("evm-key.hex"))?;
        let ports = super::config::FounderPublishedPorts::from_offset(resolved_vm.port_offset)?;
        let operator = OperatorConfig {
            version: OPERATOR_CONFIG_VERSION,
            network: OperatorNetworkConfig {
                chain_id: inputs.config.chain_id,
                certified_rpc: inputs.config.certified_rpc.clone(),
                advertise_ip: founder.advertise_ip,
                rpc_bind_ip: founder.rpc_bind_ip,
                ports: OperatorNetworkPorts::default(),
                vm_prefix: Some(resolved_vm.prefix),
                vm_gateway: Some(resolved_vm.gateway),
                vm_dns: resolved_vm.dns.clone(),
                vm_consensus_port: Some(ports.consensus_p2p_tcp),
                vm_reth_port: Some(ports.reth_p2p_tcp),
                vm_discv5_port: Some(ports.reth_discv5_udp),
                vm_radicle_port: Some(ports.radicle_tcp),
                bootnodes: inputs.bootnodes.to_vec(),
                consensus_peers: consensus_peers.clone(),
                use_local_defaults: inputs.config.topology.use_local_defaults,
            },
            feeder: OperatorFeederConfig {
                public_toml: feeder_public_toml.clone(),
            },
            hyperlane: inputs.config.resolve_hyperlane_config(founder)?,
        };
        operator.validate()?;
        operator.write_runtime_config_for_role(
            &tree,
            validator_addr,
            outbe_operator_node_role::NodeRole::Founder,
        )?;
        let hyperlane_public = fs::read_to_string(tree.join("hyperlane-public.toml"))?;
        outbe_operator_node_role::config::validate_hyperlane_public_toml(
            &hyperlane_public,
            inputs.config.chain_id,
            true,
        )?;
        for name in [
            "validator.env",
            "feeder-public.toml",
            "hyperlane-public.toml",
        ] {
            fs::set_permissions(tree.join(name), fs::Permissions::from_mode(0o640))?;
        }

        let has_authorized_keys = if let Some(configured) = &resolved_vm.authorized_keys {
            let source = resolve_relative_path(inputs.config_path, configured);
            ensure!(
                source.is_file(),
                "vm.authorized_keys does not exist for {}: {}",
                founder.name,
                source.display()
            );
            let dev_dir = tree.join("dev");
            fs::create_dir_all(&dev_dir)?;
            fs::set_permissions(&dev_dir, fs::Permissions::from_mode(0o700))?;
            copy_file_mode(&source, &dev_dir.join("authorized_keys"), 0o600)?;
            true
        } else {
            false
        };

        artifacts.push(FounderArtifact {
            index,
            name: founder.name.clone(),
            ssh_host: None,
            advertise_ip: founder.advertise_ip,
            validator_address: required_validator_field(validator, "address")?.to_owned(),
            hyperlane_address: required_validator_field(validator, "hyperlane_address")?.to_owned(),
            consensus_public_key: required_validator_field(validator, "public_key")?.to_owned(),
            archive: None,
            archive_sha256: None,
            vm: Some(FounderVmArtifact {
                data_tree: relative,
                vmm_host: resolved_vm.vmm_host.host,
                vmm_user: resolved_vm.vmm_host.user,
                vmm_port: resolved_vm.vmm_host.port,
                bridge_ip: resolved_vm.bridge_ip,
                port_offset: resolved_vm.port_offset,
                vcpu: resolved_vm.vcpu,
                memory_gib: resolved_vm.memory_gib,
                disk_gib: resolved_vm.disk_gib,
                flavor: resolved_vm.flavor,
                has_authorized_keys,
            }),
        });
    }
    Ok(artifacts)
}

fn consensus_peers(config: &NetworkConfig, validators: &[Value]) -> Result<Vec<String>> {
    config
        .founders
        .iter()
        .zip(validators)
        .map(|(founder, validator)| {
            let pubkey = required_validator_field(validator, "public_key")?;
            let p2p_address = required_validator_field(validator, "p2p_address")?;
            let port_offset = if config.vm.is_some() {
                config.resolve_founder_vm(founder)?.port_offset
            } else {
                0
            };
            if port_offset == 0 {
                return Ok(format!("{pubkey}@{p2p_address}"));
            }
            let (host, port_str) = p2p_address.rsplit_once(':').ok_or_else(|| {
                eyre::eyre!(
                    "p2p_address `{p2p_address}` missing port for {}",
                    founder.name
                )
            })?;
            let base_port: u16 = port_str.parse().wrap_err_with(|| {
                format!(
                    "invalid port in p2p_address `{p2p_address}` for {}",
                    founder.name
                )
            })?;
            // `genesis_cli_config` already encodes `{advertise_ip}:{30400 + port_offset}` in
            // `validators[]`, so avoid double-offsetting if `p2p_address` already carries it.
            let expected_offset_port = super::config::BASE_CONSENSUS_P2P_PORT
                .checked_add(port_offset)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "port_offset {port_offset} overflows consensus port for {}",
                        founder.name
                    )
                })?;
            let effective_port = if base_port == expected_offset_port {
                base_port
            } else {
                base_port.checked_add(port_offset).ok_or_else(|| {
                    eyre::eyre!(
                        "port_offset {port_offset} overflows consensus port {base_port} for {}",
                        founder.name
                    )
                })?
            };
            Ok(format!("{pubkey}@{host}:{effective_port}"))
        })
        .collect()
}

fn resolve_relative_path(config_path: &Path, configured: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(configured)
    }
}

fn copy_file_mode(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn copy_vm_keys(source: &Path, destination: &Path) -> Result<()> {
    copy_tree(source, destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    for entry in WalkDir::new(destination).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
        } else {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600))?;
        }
    }
    let p2p_key = destination.join("reth-p2p-secret.hex");
    if p2p_key.is_file() {
        let normalized: String = fs::read_to_string(&p2p_key)?
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        fs::write(&p2p_key, normalized.as_bytes())?;
        fs::set_permissions(&p2p_key, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub(super) fn build_joining_artifacts(artifacts: &BuildArtifacts<'_>) -> Result<JoiningArtifact> {
    let joining = artifacts.root.join("joining");
    fs::create_dir(&joining)?;
    let files = joining_bundle_files(artifacts)?;
    let bundle_path = joining.join("outbe-validator-operator-bundle.tar.zst");
    bundle::create_archive(&bundle_path, files)?;

    let bootnodes = fs::read_to_string(artifacts.root.join("reth-bootnodes.txt"))?
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let peers = consensus_peers(artifacts.config, artifacts.validators)?;
    let operator =
        joining_operator_config(artifacts.config, artifacts.software_root, bootnodes, peers)?;
    let config_path = joining.join("operator-config.example.yaml");
    fs::write(&config_path, serde_yaml_ng::to_string(&operator)?)?;
    Ok(JoiningArtifact {
        bundle: "joining/outbe-validator-operator-bundle.tar.zst".into(),
        bundle_sha256: sha256_file(&bundle_path)?,
        config_example: "joining/operator-config.example.yaml".into(),
    })
}

fn joining_operator_config(
    config: &NetworkConfig,
    software: &Path,
    bootnodes: Vec<String>,
    consensus_peers: Vec<String>,
) -> Result<OperatorConfig> {
    let (vm_prefix, vm_gateway, vm_dns) = match &config.vm {
        Some(vm) => (vm.prefix, vm.gateway, vm.dns.clone()),
        None => (None, None, Vec::new()),
    };
    Ok(OperatorConfig {
        version: OPERATOR_CONFIG_VERSION,
        network: OperatorNetworkConfig {
            chain_id: config.chain_id,
            certified_rpc: config.certified_rpc.clone(),
            advertise_ip: Ipv4Addr::new(192, 0, 2, 10),
            rpc_bind_ip: Ipv4Addr::LOCALHOST,
            ports: OperatorNetworkPorts::default(),
            vm_prefix,
            vm_gateway,
            vm_dns,
            vm_consensus_port: None,
            vm_reth_port: None,
            vm_discv5_port: None,
            vm_radicle_port: None,
            bootnodes,
            consensus_peers,
            use_local_defaults: config.topology.use_local_defaults,
        },
        feeder: OperatorFeederConfig {
            public_toml: joining_feeder_public_toml(config, software)?,
        },
        hyperlane: config.resolve_joining_hyperlane_config()?,
    })
}

fn feeder_pairs_path(config: &NetworkConfig, software: &Path) -> PathBuf {
    let profile = match config.network {
        Network::Devnet => "devnet",
        Network::Testnet | Network::Mainnet => "testnet",
    };
    software
        .join("share/outbe-genesis/profiles")
        .join(profile)
        .join("pairs.toml")
}

fn joining_feeder_public_toml(config: &NetworkConfig, software: &Path) -> Result<String> {
    let pairs_path = feeder_pairs_path(config, software);
    let pairs = fs::read_to_string(&pairs_path)
        .wrap_err_with(|| format!("read feeder pairs: {}", pairs_path.display()))?;
    let mut public =
        String::from("[oracle]\nsources = \"local\"\nvote_period = 60\npoll_interval_secs = 2\n");
    if config.network != Network::Devnet {
        let rest = config
            .genesis
            .overrides
            .get("price_feed_rest")
            .and_then(serde_yaml_ng::Value::as_str)
            .unwrap_or("https://prc.testnet.outbe.net");
        let rest = serde_json::to_string(rest)?;
        public.push_str(&format!(
            "\n[[api_providers]]\nname = \"mock_http\"\nrest = {rest}\n"
        ));
        public.push_str(&outbe_genesis_cli::launch_bundle::feeder_pairs_settings(
            &json!({"feeder_pairs": pairs_path}),
        )?);
        public.push('\n');
    }
    public.push('\n');
    public.push_str(&pairs);
    Ok(public)
}

fn joining_bundle_files(artifacts: &BuildArtifacts<'_>) -> Result<Vec<BundleInput>> {
    let mut files = Vec::new();
    for file in &artifacts.software.files {
        let destination = match file.path.strip_prefix("bin/") {
            Some(name) => Some(name.to_owned()),
            None if file.path == ".bb-crs/bn254_g1.dat" => Some(file.path.clone()),
            None => None,
        };
        if let Some(path) = destination {
            files.push(BundleInput {
                path,
                source: artifacts.software_root.join(&file.path),
                mode: file.mode,
            });
        }
    }
    files.push(BundleInput {
        path: "genesis.json".into(),
        source: artifacts.root.join("genesis.json"),
        mode: 0o644,
    });
    Ok(files)
}

pub(super) fn required_validator_field<'a>(validator: &'a Value, name: &str) -> Result<&'a str> {
    validator
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated validator has no {name}"))
}

pub(super) fn verify_devnet_hyperlane_admin(
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

fn read_hyperlane_admin_slot(genesis: &Value) -> Result<Address> {
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

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    ensure!(
        source.is_dir(),
        "keys directory does not exist: {}",
        source.display()
    );
    fs::create_dir_all(destination)?;
    for entry in WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir(&target)?;
        } else {
            ensure!(
                entry.file_type().is_file(),
                "keys directory contains a non-file entry"
            );
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, entry.metadata()?.permissions())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, U256};
    use outbe_primitives::tee_genesis_v1::{
        initial_tee_policy_v1, tee_attestation_v1_genesis_field, InitialTeeProfileV1,
    };

    fn feeder_keys(root: &Path, count: usize) -> (PathBuf, Vec<String>) {
        let keys = root.join("keys");
        let mut addresses = Vec::new();
        for index in 0..count {
            let dir = keys.join(format!("validator-{index}"));
            fs::create_dir_all(&dir).unwrap();
            let feeder_key = dir.join("feeder-key.hex");
            fs::write(&feeder_key, format!("{:064x}", index + 1)).unwrap();
            let evm_key = dir.join("evm-key.hex");
            fs::write(&evm_key, format!("{:064x}", index + 101)).unwrap();
            addresses.push(format!("{:#x}", validator_address(&feeder_key).unwrap()));
        }
        (keys, addresses)
    }

    #[test]
    fn genesis_cli_config_carries_prefunds_and_runtime_axes() {
        let temp = tempfile::tempdir().unwrap();
        let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        let (keys, feeder_addresses) = feeder_keys(temp.path(), config.founders.len());
        let value = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
        assert_eq!(value["network"], "testnet");
        assert_eq!(value["tee"]["mode"], "no-attestation-dev");
        outbe_genesis_cli::network::validate_config(&value).unwrap();
        assert_eq!(
            value["balance"]["0x1111111111111111111111111111111111111111"],
            "1000000000000000000000"
        );
        for address in feeder_addresses {
            assert_eq!(value["balance"][address], FEEDER_FUNDING_WEI.to_string());
        }
    }

    #[test]
    fn mainnet_rejects_generated_unattested_policy() {
        let temp = tempfile::tempdir().unwrap();
        let mut config: NetworkConfig =
            serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        config.network = Network::Mainnet;
        config.chain_id = Network::Mainnet.chain_id();
        config.validate().unwrap();
        let (keys, _) = feeder_keys(temp.path(), config.founders.len());
        let value = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
        let error = outbe_genesis_cli::network::validate_config(&value).unwrap_err();
        assert!(error.to_string().contains("TDX/SNP attestation"), "{error}");
    }

    #[test]
    fn reads_genesis_block_hash_and_network_binding() {
        let hash = B256::repeat_byte(0x42);
        let policy =
            initial_tee_policy_v1(InitialTeeProfileV1::NoAttestationDev, 424_242, hash).unwrap();
        let genesis = json!({
            "config": {
                "teeAttestationV1": tee_attestation_v1_genesis_field(&policy).unwrap()
            }
        });
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), serde_json::to_vec(&genesis).unwrap()).unwrap();
        let (actual, binding) = genesis_identity(temp.path()).unwrap();
        assert_eq!(actual, format!("{hash:#x}"));
        assert_eq!(binding.genesis_hash, hash);
        assert_eq!(binding.chain_id, U256::from(424_242).to_be_bytes::<32>());
    }

    #[test]
    fn vm_multi_host_port_offset_applies_to_bootnodes_and_consensus_peers() {
        let temp = tempfile::tempdir().unwrap();
        let software_root = temp.path().join("software");
        let staging_root = temp.path().join("staging");
        let keys_dir = staging_root.join("secrets/founder-keys");
        let fake_bins = temp.path().join("bins");
        fs::create_dir_all(&staging_root).unwrap();
        fs::create_dir_all(&fake_bins).unwrap();

        let fake_keygen = fake_bins.join("outbe-keygen");
        fs::write(&fake_keygen, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();

        let pairs = software_root.join("share/outbe-genesis/profiles/testnet/pairs.toml");
        fs::create_dir_all(pairs.parent().unwrap()).unwrap();
        fs::write(
            &pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock_http\" }]\n",
        )
        .unwrap();
        let crs = software_root.join(".bb-crs/bn254_g1.dat");
        fs::create_dir_all(crs.parent().unwrap()).unwrap();
        fs::write(&crs, b"crs-bytes").unwrap();
        fs::write(
            staging_root.join("genesis.json"),
            br#"{"config":{"chainId":54322345}}"#,
        )
        .unwrap();

        let config_path = temp.path().join("network.yaml");
        let config: NetworkConfig = serde_yaml_ng::from_str(
            r#"
version: 1
name: mixed-testnet
network: testnet
chain_id: 54322345
certified_rpc: "http://10.77.0.10:8545"
vm:
  vmm_host:
    host: 162.19.107.121
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
founders:
  - name: validator-0
    advertise_ip: 162.19.107.121
    rpc_bind_ip: 10.77.0.10
    hyperlane_public_url: http://162.19.107.121:9000
    vm:
      bridge_ip: 10.77.0.10
      port_offset: 0
  - name: validator-1
    advertise_ip: 162.19.107.121
    rpc_bind_ip: 10.77.0.11
    hyperlane_public_url: http://162.19.107.121:9001
    vm:
      bridge_ip: 10.77.0.11
      port_offset: 1
  - name: validator-2
    advertise_ip: 141.95.168.110
    rpc_bind_ip: 10.77.0.10
    hyperlane_public_url: http://141.95.168.110:9000
    vm:
      bridge_ip: 10.77.0.10
      port_offset: 0
      vmm_host:
        host: 141.95.168.110
  - name: validator-3
    advertise_ip: 141.95.168.110
    rpc_bind_ip: 10.77.0.11
    hyperlane_public_url: http://141.95.168.110:9001
    vm:
      bridge_ip: 10.77.0.11
      port_offset: 1
      vmm_host:
        host: 141.95.168.110
hyperlane:
  gasless: true
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {}
"#,
        )
        .unwrap();
        config.validate().unwrap();

        let mut validators = Vec::new();
        for (index, founder) in config.founders.iter().enumerate() {
            let val_keys = keys_dir.join(format!("validator-{index}"));
            fs::create_dir_all(val_keys.join("radicle/keys")).unwrap();
            let key_hex = format!("{:064x}", index + 1);
            for file in [
                "signing-key.hex",
                "reth-p2p-secret.hex",
                "radicle/keys/radicle",
            ] {
                fs::write(val_keys.join(file), format!("{key_hex}\n")).unwrap();
            }
            for file in ["evm-key.hex", "feeder-key.hex", "hyperlane-key.hex"] {
                fs::write(val_keys.join(file), &key_hex).unwrap();
            }
            let addr = format!(
                "{:#x}",
                validator_address(&val_keys.join("evm-key.hex")).unwrap()
            );
            validators.push(json!({
                "address": addr,
                "hyperlane_address": addr,
                "public_key": format!("{:096x}", index + 1),
                "p2p_address": format!("{}:30400", founder.advertise_ip),
            }));
        }

        let bootnodes =
            write_vm_bootnodes(&json!({}), &config, &validators, &keys_dir, &staging_root).unwrap();
        assert!(
            bootnodes[0].ends_with("@162.19.107.121:30303"),
            "{bootnodes:?}"
        );
        assert!(
            bootnodes[1].ends_with("@162.19.107.121:30304"),
            "{bootnodes:?}"
        );
        assert!(
            bootnodes[2].ends_with("@141.95.168.110:30303"),
            "{bootnodes:?}"
        );
        assert!(
            bootnodes[3].ends_with("@141.95.168.110:30304"),
            "{bootnodes:?}"
        );

        let artifacts = emit_vm_founder_trees(&VmTreeInputs {
            staging_root: &staging_root,
            software_root: &software_root,
            keys_dir: &keys_dir,
            config_path: &config_path,
            config: &config,
            validators: &validators,
            bootnodes: &bootnodes,
        })
        .unwrap();
        assert_eq!(artifacts[0].vm.as_ref().unwrap().port_offset, 0);
        assert_eq!(artifacts[1].vm.as_ref().unwrap().port_offset, 1);
        assert_eq!(artifacts[2].vm.as_ref().unwrap().port_offset, 0);
        assert_eq!(artifacts[3].vm.as_ref().unwrap().port_offset, 1);

        let genesis_cfg = genesis_cli_config(&config, &software_root, &keys_dir, None).unwrap();
        assert_eq!(
            genesis_cfg["validators"],
            json!([
                "162.19.107.121",
                "162.19.107.121:30401",
                "141.95.168.110",
                "141.95.168.110:30401",
            ])
        );

        let env0 = fs::read_to_string(staging_root.join("vm/validator-0/validator.env")).unwrap();
        assert!(
            env0.contains("OUTBE_EXTERNAL_IP=\"162.19.107.121\""),
            "{env0}"
        );
        assert!(env0.contains("OUTBE_RPC_BIND_IP=\"10.77.0.10\""), "{env0}");
        assert!(env0.contains("OUTBE_VM_PREFIX=\"24\""), "{env0}");
        assert!(env0.contains("OUTBE_VM_GATEWAY=\"10.77.0.1\""), "{env0}");
        assert!(env0.contains("OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\""), "{env0}");
        assert!(env0.contains("OUTBE_CONSENSUS_PORT=\"30400\""), "{env0}");
        assert!(env0.contains("OUTBE_RETH_PORT=\"30303\""), "{env0}");
        assert!(env0.contains("OUTBE_DISCV5_PORT=\"31303\""), "{env0}");
        assert!(env0.contains("OUTBE_RADICLE_PORT=\"8776\""), "{env0}");
        assert!(env0.contains("162.19.107.121:30400"), "{env0}");
        assert!(env0.contains("162.19.107.121:30401"), "{env0}");
        assert!(env0.contains("141.95.168.110:30400"), "{env0}");
        assert!(env0.contains("141.95.168.110:30401"), "{env0}");

        let env1 = fs::read_to_string(staging_root.join("vm/validator-1/validator.env")).unwrap();
        assert!(env1.contains("OUTBE_CONSENSUS_PORT=\"30401\""), "{env1}");
        assert!(env1.contains("OUTBE_RETH_PORT=\"30304\""), "{env1}");
        assert!(env1.contains("OUTBE_DISCV5_PORT=\"31304\""), "{env1}");
        assert!(env1.contains("OUTBE_RADICLE_PORT=\"8777\""), "{env1}");

        for index in 0..4 {
            let tree = staging_root.join(format!("vm/validator-{index}"));
            outbe_operator_node_role::vm_boot::verify_tree_without_mount(&tree, &fake_bins)
                .unwrap();
        }
    }

    #[test]
    fn genesis_cli_config_defaults_epoch_and_dkg_windows_when_omitted() {
        let temp = tempfile::tempdir().unwrap();
        let mut config: NetworkConfig =
            serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        config.genesis.epoch_length_blocks = None;
        config.genesis.dkg_prepare_window_blocks = None;
        config.genesis.dkg_activation_grace_blocks = None;
        config.validate().unwrap();

        let (keys, _) = feeder_keys(temp.path(), config.founders.len());
        let defaulted = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
        assert_eq!(defaulted["epoch_length_blocks"], 1200);
        assert_eq!(defaulted["dkg_prepare_window_blocks"], 300);
        assert_eq!(defaulted["dkg_activation_grace_blocks"], 300);
        outbe_genesis_cli::network::validate_config(&defaulted).unwrap();

        // Explicit valid values (>= 120) are preserved verbatim.
        config.genesis.epoch_length_blocks = Some(2400);
        config.genesis.dkg_prepare_window_blocks = Some(120);
        config.genesis.dkg_activation_grace_blocks = Some(180);
        config.validate().unwrap();
        let explicit = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
        assert_eq!(explicit["epoch_length_blocks"], 2400);
        assert_eq!(explicit["dkg_prepare_window_blocks"], 120);
        assert_eq!(explicit["dkg_activation_grace_blocks"], 180);

        // When `base_genesis` is provided, epoch/DKG defaults are NOT injected.
        config.genesis.epoch_length_blocks = None;
        config.genesis.dkg_prepare_window_blocks = None;
        config.genesis.dkg_activation_grace_blocks = None;
        let base_path = temp.path().join("base-genesis.json");
        let with_base = genesis_cli_config(&config, temp.path(), &keys, Some(&base_path)).unwrap();
        assert!(with_base.get("epoch_length_blocks").is_none());
        assert!(with_base.get("dkg_prepare_window_blocks").is_none());
        assert!(with_base.get("dkg_activation_grace_blocks").is_none());
    }
}
