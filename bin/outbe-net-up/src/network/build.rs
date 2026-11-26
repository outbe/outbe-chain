#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;

use std::{
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use outbe_genesis_cli::{
    attestation::run_tee_stage, create_genesis::create_seeded_genesis, launch_bundle,
};
use outbe_operator_node_role::{process, progress::Progress, validator_address};
use outbe_software_bundle::{sha256_file, verify_and_extract, SoftwareManifest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use walkdir::WalkDir;

use super::{
    build_genesis as genesis, build_joining as joining, build_vm as vm_artifacts,
    config::{Network, NetworkConfig, VmFlavor},
};

#[cfg(test)]
pub(super) use genesis::verify_devnet_hyperlane_admin;
pub(super) use genesis::{genesis_cli_config, genesis_identity};
pub(super) use joining::build_joining_artifacts;
pub(super) use vm_artifacts::{build_vm_firewall_mappings, render_firewall_mappings_table};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) firewall_mappings: Vec<FirewallMappingEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct FirewallMappingEntry {
    pub(crate) member: String,
    pub(crate) host: String,
    pub(crate) public_ip: Ipv4Addr,
    pub(crate) public_port: u16,
    pub(crate) proto: String,
    pub(crate) vm_ip: Ipv4Addr,
    pub(crate) vm_port: u16,
    pub(crate) purpose: String,
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

const fn default_rpc_port() -> u16 {
    super::config::BASE_RPC_PORT
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct FounderArtifact {
    pub(crate) index: usize,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ssh_host: Option<String>,
    pub(super) advertise_ip: Ipv4Addr,
    #[serde(default = "default_rpc_port")]
    pub(crate) rpc_port: u16,
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
    pub(super) vm_ip: Ipv4Addr,
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
    let base_genesis = genesis::stage_base_genesis(config_path, &config, staging.path())?;

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
    let (seeded, final_genesis, validators) = build_genesis_files(
        &config,
        software_root.path(),
        &keys,
        staging.path(),
        &genesis_config,
    )?;

    let deployment = if config.vm.is_some() {
        DeploymentKind::Vm
    } else {
        DeploymentKind::Host
    };
    let founders = assemble_founders(
        deployment,
        &mut progress,
        &AssembleFoundersInputs {
            staging_root: staging.path(),
            software_root: software_root.path(),
            keys_dir: &keys,
            config_path,
            config: &config,
            genesis_config: &genesis_config,
            final_genesis: &final_genesis,
            validators: &validators,
        },
    )?;

    print_build_summary(&founders, &config)?;
    let firewall_mappings = build_vm_firewall_mappings(&config)?;
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
        firewall_mappings,
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

struct AssembleFoundersInputs<'a> {
    staging_root: &'a Path,
    software_root: &'a Path,
    keys_dir: &'a Path,
    config_path: &'a Path,
    config: &'a NetworkConfig,
    genesis_config: &'a Value,
    final_genesis: &'a Path,
    validators: &'a [Value],
}

fn build_genesis_files(
    config: &NetworkConfig,
    software_root: &Path,
    keys: &Path,
    staging: &Path,
    genesis_config: &Value,
) -> Result<(PathBuf, PathBuf, Vec<Value>)> {
    let seeded = staging.join("genesis.seeded.json");
    let keygen = software_root.join("bin/outbe-keygen");
    let validators = create_seeded_genesis(
        genesis_config,
        keys,
        keygen
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
        &seeded,
    )
    .wrap_err("failed to create seeded genesis")?;
    verify_devnet_admin_if_needed(config, keys, &seeded)?;

    let final_genesis = staging.join("genesis.json");
    run_tee_stage(
        software_root
            .join("bin/outbe-chain")
            .to_str()
            .ok_or_else(|| eyre::eyre!("software path is not UTF-8"))?,
        &seeded,
        &final_genesis,
        genesis_config,
    )
    .wrap_err("failed to finalize the genesis TEE policy")?;
    verify_devnet_admin_if_needed(config, keys, &final_genesis)?;
    Ok((seeded, final_genesis, validators))
}

fn verify_devnet_admin_if_needed(
    config: &NetworkConfig,
    keys: &Path,
    genesis_path: &Path,
) -> Result<()> {
    if config.network == Network::Devnet {
        let founder0 = validator_address(&keys.join("validator-0/evm-key.hex"))?;
        genesis::verify_devnet_hyperlane_admin(genesis_path, founder0)?;
    }
    Ok(())
}

fn assemble_founders(
    deployment: DeploymentKind,
    progress: &mut Progress,
    inputs: &AssembleFoundersInputs<'_>,
) -> Result<Vec<FounderArtifact>> {
    match deployment {
        DeploymentKind::Host => {
            launch_bundle::render(
                inputs.genesis_config,
                inputs.validators,
                inputs.final_genesis,
                inputs.keys_dir,
            )
            .wrap_err("failed to render founder launch bundles")?;
            progress.stage("assembling founder and joining artifacts");
            founder_artifacts(inputs.staging_root, inputs.config, inputs.validators)
        }
        DeploymentKind::Vm => {
            let bootnodes = vm_artifacts::write_vm_bootnodes(
                inputs.genesis_config,
                inputs.config,
                inputs.validators,
                inputs.keys_dir,
                inputs.staging_root,
            )?;
            progress.stage("assembling VM founder data trees and joining artifacts");
            vm_artifacts::emit_vm_founder_trees(&vm_artifacts::VmTreeInputs {
                staging_root: inputs.staging_root,
                software_root: inputs.software_root,
                keys_dir: inputs.keys_dir,
                config_path: inputs.config_path,
                config: inputs.config,
                validators: inputs.validators,
                bootnodes: &bootnodes,
            })
        }
    }
}

fn print_build_summary(founders: &[FounderArtifact], config: &NetworkConfig) -> Result<()> {
    println!("\nHyperlane signers:");
    for founder in founders {
        println!("  {:<20} {}", founder.name, founder.hyperlane_address);
    }
    let firewall_mappings = build_vm_firewall_mappings(config)?;
    if !firewall_mappings.is_empty() {
        println!(
            "\nDNAT / firewall mappings:\n{}",
            render_firewall_mappings_table(&firewall_mappings)
        );
    }
    Ok(())
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
                rpc_port: founder.rpc_port(),
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
