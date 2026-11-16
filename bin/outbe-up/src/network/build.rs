use std::{fs, net::Ipv4Addr, os::unix::fs::PermissionsExt, path::Path};

use eyre::{ensure, Result};
use outbe_genesis_cli::{
    attestation::run_tee_stage, create_genesis::create_seeded_genesis, launch_bundle,
};
use outbe_software_bundle::{sha256_file, verify_and_extract, SoftwareManifest};
use serde::Serialize;
use serde_json::{json, Map, Value};
use walkdir::WalkDir;

use crate::{
    support::process,
    validator::{
        bundle::{self, BundleInput},
        config::EnclaveRuntime,
    },
};

use super::config::{NetworkConfig, TeePolicy};

const MANIFEST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum BuildState {
    Complete,
    SigningRequired,
}

#[derive(Debug, Serialize)]
struct NetworkManifest {
    version: u32,
    state: BuildState,
    name: String,
    network: String,
    chain_id: u64,
    config_sha256: String,
    software: SoftwareIdentity,
    genesis: GenesisIdentity,
    founders: Vec<FounderArtifact>,
    joining: Option<JoiningArtifact>,
}

#[derive(Debug, Serialize)]
struct SoftwareIdentity {
    archive_sha256: String,
    commit: String,
    release_tag: String,
    profile: String,
    target: String,
}

#[derive(Debug, Serialize)]
struct GenesisIdentity {
    seeded_path: String,
    seeded_sha256: String,
    final_path: Option<String>,
    final_sha256: Option<String>,
    genesis_block_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct FounderArtifact {
    index: usize,
    name: String,
    ssh_host: String,
    advertise_ip: Ipv4Addr,
    validator_address: String,
    consensus_public_key: String,
    archive: Option<String>,
    archive_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct JoiningArtifact {
    bundle: String,
    bundle_sha256: String,
    config_example: String,
}

struct BuildArtifacts<'a> {
    root: &'a Path,
    software_root: &'a Path,
    software: &'a SoftwareManifest,
    config: &'a NetworkConfig,
    validators: &'a [Value],
}

pub fn run(
    software_bundle: &Path,
    config_path: &Path,
    output: &Path,
    existing_keys: Option<&Path>,
) -> Result<()> {
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
    ensure_profile(&config, &software)?;
    let signing_required = config.tee.policy == TeePolicy::DcapRequired;
    ensure!(
        signing_required || matches!(config.tee.runtime, EnclaveRuntime::Mock),
        "production Gramine direct-dev packaging needs a signed Gramine runtime; use mock for development or dcap-required for production"
    );

    let staging = tempfile::Builder::new()
        .prefix(".outbe-network-")
        .tempdir_in(parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    fs::copy(config_path, staging.path().join("network.yaml"))?;
    fs::copy(
        software_bundle,
        staging.path().join("software-bundle.tar.zst"),
    )?;

    let keys = staging.path().join("secrets/founder-keys");
    match existing_keys {
        Some(source) => copy_tree(source, &keys)?,
        None => generate_keys(&config, software_root.path(), &keys)?,
    }
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700))?;

    let legacy = legacy_config(&config, software_root.path(), &keys)?;
    let seeded = staging.path().join("genesis.seeded.json");
    let keygen = software_root.path().join("bin/outbe-keygen");
    let validators = create_seeded_genesis(
        &legacy,
        &keys,
        keygen
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
        &seeded,
    )
    .map_err(eyre::Report::msg)?;

    let final_genesis = staging.path().join("genesis.json");
    if !signing_required {
        run_tee_stage(
            software_root
                .path()
                .join("bin/outbe-chain")
                .to_str()
                .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
            &seeded,
            &final_genesis,
            &legacy,
        )
        .map_err(eyre::Report::msg)?;
        launch_bundle::render(&legacy, &validators, &final_genesis, &keys)
            .map_err(eyre::Report::msg)?;
    } else {
        write_signing_request(staging.path(), &config, &software, software_bundle, &seeded)?;
    }

    let founders = founder_artifacts(staging.path(), &config, &validators, signing_required)?;
    let joining = if signing_required {
        None
    } else {
        Some(build_joining_artifacts(&BuildArtifacts {
            root: staging.path(),
            software_root: software_root.path(),
            software: &software,
            config: &config,
            validators: &validators,
        })?)
    };
    let manifest = NetworkManifest {
        version: MANIFEST_VERSION,
        state: if signing_required {
            BuildState::SigningRequired
        } else {
            BuildState::Complete
        },
        name: config.name.clone(),
        network: config.network.as_str().to_owned(),
        chain_id: config.chain_id,
        config_sha256: sha256_file(config_path)?,
        software: SoftwareIdentity {
            archive_sha256: sha256_file(software_bundle)?,
            commit: software.commit,
            release_tag: software.release_tag,
            profile: software.profile.as_str().to_owned(),
            target: software.target,
        },
        genesis: GenesisIdentity {
            seeded_path: "genesis.seeded.json".into(),
            seeded_sha256: sha256_file(&seeded)?,
            final_path: (!signing_required).then(|| "genesis.json".into()),
            final_sha256: (!signing_required)
                .then(|| sha256_file(&final_genesis))
                .transpose()?,
            genesis_block_hash: None,
        },
        founders,
        joining,
    };
    fs::write(
        staging.path().join("network-manifest.json"),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    fs::rename(staging.keep(), output)?;
    println!("network artifacts: {}", output.display());
    if signing_required {
        println!(
            "state: signing required; run the protected SGX job, then `outbe-up network finalize`"
        );
    } else {
        println!("state: complete; ready for `outbe-up network prepare`");
    }
    Ok(())
}

fn ensure_profile(config: &NetworkConfig, software: &SoftwareManifest) -> Result<()> {
    ensure!(
        config.software_profile == software.profile,
        "network software_profile does not match the software bundle"
    );
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

fn legacy_config(config: &NetworkConfig, software: &Path, keys: &Path) -> Result<Value> {
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
        ("crs_path", json!(software.join(".bb-crs/bn254_g1.dat"))),
        (
            "consensus_use_local_defaults",
            json!(config.topology.use_local_defaults),
        ),
        ("rpc_bind_ip", json!(rpc_bind(config)?)),
        ("tee", json!({"mode": config.tee.policy.as_str()})),
        (
            "enclave_sgx",
            json!(!matches!(config.tee.runtime, EnclaveRuntime::Mock)),
        ),
    ] {
        root.insert(key.to_owned(), value);
    }
    if matches!(config.tee.runtime, EnclaveRuntime::Mock) {
        root.insert(
            "enclave_runner".into(),
            json!("/opt/outbe-chain/outbe-tee-enclave-mock"),
        );
    }
    if let Some(image) = &config.tee.enclave_image {
        root.insert("enclave_image".into(), json!(image));
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

fn founder_artifacts(
    root: &Path,
    config: &NetworkConfig,
    validators: &[Value],
    signing_required: bool,
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
                consensus_public_key: required_validator_field(validator, "public_key")?.to_owned(),
                archive: (!signing_required).then_some(relative),
                archive_sha256: (!signing_required)
                    .then(|| sha256_file(&archive))
                    .transpose()?,
            })
        })
        .collect()
}

fn build_joining_artifacts(artifacts: &BuildArtifacts<'_>) -> Result<JoiningArtifact> {
    let joining = artifacts.root.join("joining");
    fs::create_dir(&joining)?;
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
    let operator = json!({
        "version": 1,
        "network": {
            "chain_id": artifacts.config.chain_id,
            "certified_rpc": artifacts.config.certified_rpc,
            "external_ip": "192.0.2.10",
            "rpc_bind_ip": "127.0.0.1",
            "bootnodes": bootnodes,
            "consensus_peers": peers,
            "use_local_defaults": artifacts.config.topology.use_local_defaults,
        },
        "tee": {
            "runtime": artifacts.config.tee.runtime.as_env(),
            "lease_duration_seconds": "auto",
        },
        "feeder": {
            "public_toml": "[oracle]\nsources = \"local\"\nvote_period = 60\npoll_interval_secs = 2\n",
        },
    });
    let config_path = joining.join("operator-config.example.yaml");
    fs::write(&config_path, serde_yaml_ng::to_string(&operator)?)?;
    Ok(JoiningArtifact {
        bundle: "joining/outbe-validator-operator-bundle.tar.zst".into(),
        bundle_sha256: sha256_file(&bundle_path)?,
        config_example: "joining/operator-config.example.yaml".into(),
    })
}

fn required_validator_field<'a>(validator: &'a Value, name: &str) -> Result<&'a str> {
    validator
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| eyre::eyre!("generated validator has no {name}"))
}

fn write_signing_request(
    root: &Path,
    config: &NetworkConfig,
    software: &SoftwareManifest,
    software_bundle: &Path,
    seeded: &Path,
) -> Result<()> {
    let request = json!({
        "version": 1,
        "network": config.network.as_str(),
        "chain_id": config.chain_id,
        "software_commit": software.commit,
        "software_bundle_sha256": sha256_file(software_bundle)?,
        "seeded_genesis": "genesis.seeded.json",
        "seeded_genesis_sha256": sha256_file(seeded)?,
        "enclave_image": config.tee.enclave_image,
    });
    fs::write(
        root.join("sgx-signing-request.json"),
        format!("{}\n", serde_json::to_string_pretty(&request)?),
    )?;
    Ok(())
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

    #[test]
    fn resolves_per_founder_rpc_binding_without_public_wildcards() {
        let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        assert_eq!(rpc_bind(&config).unwrap(), "validator");
    }

    #[test]
    fn legacy_config_carries_prefunds_and_runtime_axes() {
        let temp = tempfile::tempdir().unwrap();
        let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
        let value = legacy_config(&config, temp.path(), &temp.path().join("keys")).unwrap();
        assert_eq!(value["network"], "testnet");
        assert_eq!(value["tee"]["mode"], "gramine-direct-dev");
        assert_eq!(value["enclave_sgx"], false);
        assert_eq!(
            value["balance"]["0x1111111111111111111111111111111111111111"],
            "1000000000000000000000"
        );
    }
}
