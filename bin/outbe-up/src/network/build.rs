use std::{fs, net::Ipv4Addr, os::unix::fs::PermissionsExt, path::Path};

use eyre::{ensure, Result};
use outbe_genesis_cli::{
    attestation::run_tee_stage, create_genesis::create_seeded_genesis, launch_bundle,
};
use outbe_primitives::tee_attestation_v1::{NetworkBindingV1, TeePolicyScheduleV1};
use outbe_software_bundle::{sha256_file, verify_and_extract, SoftwareManifest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use walkdir::WalkDir;

use crate::{
    support::{process, progress::Progress},
    validator::bundle::{self, BundleInput},
    validator::config::{
        FeederConfig as OperatorFeederConfig, NetworkConfig as OperatorNetworkConfig,
        OperatorConfig, CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
};

use super::config::NetworkConfig;

const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum BuildState {
    Ready,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct NetworkManifest {
    pub(super) version: u32,
    pub(super) state: BuildState,
    pub(super) name: String,
    pub(super) network: String,
    pub(super) chain_id: u64,
    pub(super) config_sha256: String,
    pub(super) software: SoftwareIdentity,
    pub(super) genesis: GenesisIdentity,
    pub(super) founders: Vec<FounderArtifact>,
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
pub(super) struct FounderArtifact {
    pub(super) index: usize,
    pub(super) name: String,
    pub(super) ssh_host: String,
    pub(super) advertise_ip: Ipv4Addr,
    pub(super) validator_address: String,
    pub(super) hyperlane_address: String,
    pub(super) consensus_public_key: String,
    pub(super) archive: String,
    pub(super) archive_sha256: String,
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
    .map_err(eyre::Report::msg)?;

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
    .map_err(eyre::Report::msg)?;
    launch_bundle::render(&genesis_config, &validators, &final_genesis, &keys)
        .map_err(eyre::Report::msg)?;

    progress.stage("assembling founder and joining artifacts");
    let founders = founder_artifacts(staging.path(), &config, &validators)?;
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
    println!(
        "Next: outbe-up network prepare --artifacts {}",
        output.display()
    );
    Ok(())
}

fn stage_base_genesis(
    config_path: &Path,
    config: &NetworkConfig,
    staging: &Path,
) -> Result<Option<std::path::PathBuf>> {
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
    .map_err(|error| eyre::eyre!("invalid final TEE policy schedule: {error}"))?;
    let policy = schedule
        .active_policy(1)
        .map_err(|error| eyre::eyre!("invalid final TEE policy: {error}"))?;
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
        .map(|founder| Value::String(founder.advertise_ip.to_string()))
        .collect();
    for (key, value) in [
        ("network", json!(config.network.as_str())),
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
        (
            "feeder_pairs",
            json!(software.join("share/outbe-genesis/profiles/testnet/pairs.toml")),
        ),
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
        ("hyperlane_origins", json!(config.hyperlane.origins)),
        ("crs_path", json!(software.join(".bb-crs/bn254_g1.dat"))),
        (
            "consensus_use_local_defaults",
            json!(config.topology.use_local_defaults),
        ),
        ("rpc_bind_ip", json!(rpc_bind(config)?)),
        ("tee", json!({"mode": "gramine-direct-dev"})),
        (
            "enclave_runner",
            json!("/opt/outbe-chain/outbe-tee-enclave-confidential-vm"),
        ),
    ] {
        root.insert(key.to_owned(), value);
    }
    if let Some(path) = base_genesis {
        root.insert("base_genesis".into(), json!(path));
    }
    for (key, value) in [
        ("timestamp", config.genesis.timestamp),
        ("epoch_length_blocks", config.genesis.epoch_length_blocks),
        (
            "dkg_prepare_window_blocks",
            config.genesis.dkg_prepare_window_blocks,
        ),
        (
            "dkg_activation_grace_blocks",
            config.genesis.dkg_activation_grace_blocks,
        ),
    ] {
        if let Some(value) = value {
            root.insert(key.into(), json!(value));
        }
    }
    if !config.genesis.prefunded_accounts.is_empty() {
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
    ensure!(
        config
            .founders
            .iter()
            .all(|founder| founder.rpc_bind_ip == first),
        "founder rpc_bind_ip values must all match or equal each founder advertise_ip"
    );
    Ok(first.to_string())
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
            Ok(FounderArtifact {
                index,
                name: founder.name.clone(),
                ssh_host: founder.ssh.host.clone(),
                advertise_ip: founder.advertise_ip,
                validator_address: required_validator_field(validator, "address")?.to_owned(),
                hyperlane_address: required_validator_field(validator, "hyperlane_address")?
                    .to_owned(),
                consensus_public_key: required_validator_field(validator, "public_key")?.to_owned(),
                archive: relative,
                archive_sha256: sha256_file(&archive)?,
            })
        })
        .collect()
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
    let peers = artifacts
        .validators
        .iter()
        .map(|validator| {
            Ok(format!(
                "{}@{}",
                required_validator_field(validator, "public_key")?,
                required_validator_field(validator, "p2p_address")?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let operator = joining_operator_config(artifacts.config, bootnodes, peers);
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
    bootnodes: Vec<String>,
    consensus_peers: Vec<String>,
) -> OperatorConfig {
    OperatorConfig {
        version: OPERATOR_CONFIG_VERSION,
        network: OperatorNetworkConfig {
            chain_id: config.chain_id,
            certified_rpc: config.certified_rpc.clone(),
            advertise_ip: Ipv4Addr::new(192, 0, 2, 10),
            rpc_bind_ip: Ipv4Addr::LOCALHOST,
            bootnodes,
            consensus_peers,
            use_local_defaults: config.topology.use_local_defaults,
        },
        feeder: OperatorFeederConfig {
            public_toml:
                "[oracle]\nsources = \"local\"\nvote_period = 60\npoll_interval_secs = 2\n".into(),
        },
        hyperlane: config
            .hyperlane
            .for_public_url("http://192.0.2.10:9000".into()),
    }
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

pub(super) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
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

    #[test]
    fn resolves_per_founder_rpc_binding_without_public_wildcards() {
        let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        assert_eq!(rpc_bind(&config).unwrap(), "validator");
    }

    #[test]
    fn genesis_cli_config_carries_prefunds_and_runtime_axes() {
        let temp = tempfile::tempdir().unwrap();
        let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        let value =
            genesis_cli_config(&config, temp.path(), &temp.path().join("keys"), None).unwrap();
        assert_eq!(value["network"], "testnet");
        assert_eq!(value["tee"]["mode"], "gramine-direct-dev");
        assert!(value.get("enclave_sgx").is_none());
        assert!(value.get("enclave_runtime").is_none());
        outbe_genesis_cli::network::validate_config(&value).unwrap();
        assert_eq!(
            value["balance"]["0x1111111111111111111111111111111111111111"],
            "1000000000000000000000"
        );
    }

    #[test]
    fn reads_genesis_block_hash_and_network_binding() {
        let hash = B256::repeat_byte(0x42);
        let policy =
            initial_tee_policy_v1(InitialTeeProfileV1::GramineDirectDev, 424_242, hash).unwrap();
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
    fn joining_config_serializes_numeric_fields_as_yaml_numbers() {
        let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        let operator = joining_operator_config(
            &config,
            vec!["enode://bootnode".into()],
            vec!["public-key@127.0.0.1:30400".into()],
        );
        let yaml = serde_yaml_ng::to_string(&operator).unwrap();
        assert!(yaml.contains("version: 1"));
        assert!(yaml.contains(&format!("chain_id: {}", config.chain_id)));
        assert!(!yaml.contains("$serde_json"));
        assert!(!yaml.contains("poll_interval_secs:"));
        assert!(!yaml.contains("signer_funding_wei:"));
        assert!(!yaml.contains("rpc_consensus:"));
        assert!(!yaml.contains("interval_secs:"));
        assert!(!yaml.contains("rpc_urls: ["));
        let decoded: OperatorConfig = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(decoded.network.chain_id, config.chain_id);
        assert_eq!(decoded.hyperlane.poll_interval_secs, 30);
        assert_eq!(decoded.hyperlane.signer_funding_wei, "1000000000000000000");
        assert!(decoded
            .hyperlane
            .origins
            .iter()
            .all(|origin| origin.interval_secs == 15));
    }

    #[test]
    fn validates_a_custom_pre_tee_base_genesis() {
        let genesis = json!({
            "config": {"chainId": 54_322_345},
            "alloc": {},
            "timestamp": "0x1"
        });
        ensure_base_genesis(&genesis, 54_322_345).unwrap();
        assert!(ensure_base_genesis(&genesis, 676).is_err());

        let mut finalized = genesis;
        finalized["config"]["teeAttestationV1"] = json!({});
        assert!(ensure_base_genesis(&finalized, 54_322_345).is_err());
    }
}
