//! Local member key generation, joining-validator data-tree tar assembly, and full-node config.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use alloy_primitives::Address;
use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    config::{
        FullNodeConfig, FullNodeNetworkConfig, NodeRole, OperatorConfig,
        CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
    vm_network::{
        DEFAULT_CONSENSUS_PORT, DEFAULT_DISCV5_PORT, DEFAULT_RADICLE_PORT, DEFAULT_RETH_PORT,
    },
};
use outbe_software_bundle::verify_and_extract;
use walkdir::WalkDir;

use super::super::{build_founder_tar_bytes, DesiredMember};
use crate::network::{config::FounderPublishedPorts, remote::NetworkArtifacts};

const REQUIRED_MEMBER_KEYS: &[&str] = &[
    "signing-key.hex",
    "evm-key.hex",
    "feeder-key.hex",
    "hyperlane-key.hex",
    "reth-p2p-secret.hex",
    "radicle/keys/radicle",
];

/// Ensure member keys exist under `<artifacts>/secrets/members/<name>/` (`0700`/`0600`).
///
/// Never deletes an existing non-empty member keys directory (F5): if all required keys are
/// present they are reused; if the directory is partially populated, fails closed naming the
/// missing file(s) so an already-staked identity can never be silently overwritten.
pub(crate) fn ensure_local_member_keys(
    artifacts: &Path,
    name: &str,
    chain_id: u64,
) -> Result<PathBuf> {
    let secrets_members = artifacts.join("secrets/members");
    fs::create_dir_all(&secrets_members)?;
    fs::set_permissions(&secrets_members, fs::Permissions::from_mode(0o700))?;
    let dir = secrets_members.join(name);

    match fs::symlink_metadata(&dir) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_dir() && !meta.file_type().is_symlink(),
                "refusing non-directory member keys path: {}",
                dir.display()
            );
            let missing: Vec<&str> = REQUIRED_MEMBER_KEYS
                .iter()
                .copied()
                .filter(|rel| !dir.join(rel).is_file())
                .collect();
            if missing.is_empty() {
                normalize_local_keys(&dir)?;
                return Ok(dir);
            }
            let is_empty = fs::read_dir(&dir)?.next().is_none();
            ensure!(
                is_empty,
                "refusing to regenerate keys into partial member keys directory {} (missing required key file(s): {}); restore the missing file(s) or remove the directory manually if no stake/VM exists",
                dir.display(),
                missing.join(", ")
            );
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&dir)?;
        }
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("stat member keys dir {}", dir.display()));
        }
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;

    let bundle = artifacts.join("software-bundle.tar.zst");
    let tmp = tempfile::Builder::new()
        .prefix("outbe-keygen-member-")
        .tempdir()
        .wrap_err("create temp dir for bundled outbe-keygen")?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
    verify_and_extract(&bundle, tmp.path())
        .wrap_err("verify software bundle for outbe-keygen extraction")?;
    let keygen = tmp.path().join("bin/outbe-keygen");
    ensure!(
        keygen.is_file(),
        "software bundle {} is missing bin/outbe-keygen",
        bundle.display()
    );
    let dir_str = dir
        .to_str()
        .ok_or_else(|| eyre::eyre!("non-UTF-8 member keys path: {}", dir.display()))?;
    // Suppress stdout/stderr so keygen output is never buffered or logged.
    let status = Command::new(&keygen)
        .args([
            "validator",
            "--output-dir",
            dir_str,
            "--chain-id",
            &chain_id.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .wrap_err_with(|| format!("execute {}", keygen.display()))?;
    ensure!(
        status.success(),
        "outbe-keygen validator failed with {status}"
    );
    normalize_local_keys(&dir)?;
    Ok(dir)
}

fn normalize_local_keys(keys_dir: &Path) -> Result<()> {
    fs::set_permissions(keys_dir, fs::Permissions::from_mode(0o700))?;
    for entry in WalkDir::new(keys_dir).follow_links(false) {
        let entry = entry?;
        let mode = if entry.file_type().is_dir() {
            0o700
        } else {
            0o600
        };
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode))?;
    }
    let p2p_key = keys_dir.join("reth-p2p-secret.hex");
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

pub(super) fn build_joining_validator_tar_bytes(
    build: &NetworkArtifacts,
    member: &DesiredMember,
    local_keys_dir: &Path,
    validator_addr: Address,
    ports: FounderPublishedPorts,
) -> Result<Vec<u8>> {
    let mut op_cfg = load_joining_operator_template(build)?;
    op_cfg.network.advertise_ip = member.advertise_ip;
    op_cfg.network.vm_ip = Some(member.service_ip);
    op_cfg.network.rpc_bind_ip = Some(member.service_ip);
    op_cfg.network.ports.rpc = member.rpc_port;
    op_cfg.network.vm_rpc_port = Some(member.rpc_port);
    op_cfg.network.vm_consensus_port = Some(ports.consensus_p2p_tcp);
    op_cfg.network.vm_reth_port = Some(ports.reth_p2p_tcp);
    op_cfg.network.vm_discv5_port = Some(ports.reth_discv5_udp);
    op_cfg.network.vm_radicle_port = Some(ports.radicle_tcp);
    op_cfg.hyperlane = build.config.resolve_hyperlane_for_endpoint(
        format!("http://{}:{}", member.advertise_ip, ports.hyperlane_tcp),
        member.service_ip,
        member.rpc_port,
    )?;
    op_cfg.validate()?;

    let tmp = tempfile::Builder::new()
        .prefix("outbe-join-tree-")
        .tempdir()
        .wrap_err("create temp dir for joining validator tree")?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
    let tree = tmp.path().join("tree");
    fs::create_dir_all(&tree)?;
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o755))?;

    let genesis_src = build.root.join(&build.manifest.genesis.final_path);
    copy_file_mode(&genesis_src, &tree.join("genesis.json"), 0o644)?;
    let crs_src = find_crs_file(&build.root)?;
    copy_file_mode(&crs_src, &tree.join(".bb-crs/bn254_g1.dat"), 0o644)?;

    let dst_keys = tree.join("keys");
    copy_keys_tree(local_keys_dir, &dst_keys)?;

    op_cfg.write_runtime_config_for_role(&tree, validator_addr, NodeRole::JoiningValidator)?;
    for name in [
        "validator.env",
        "feeder-public.toml",
        "hyperlane-public.toml",
        "reth.toml",
    ] {
        fs::set_permissions(tree.join(name), fs::Permissions::from_mode(0o640))?;
    }

    build_founder_tar_bytes(&tree)
}

pub(super) fn build_full_node_config(
    build: &NetworkArtifacts,
    member: &DesiredMember,
) -> Result<FullNodeConfig> {
    let op_cfg = load_joining_operator_template(build)?;
    let cfg = FullNodeConfig {
        version: OPERATOR_CONFIG_VERSION,
        network: FullNodeNetworkConfig {
            chain_id: build.config.chain_id,
            certified_rpc: build.config.certified_rpc.clone(),
            advertise_ip: member.advertise_ip,
            rpc_bind_ip: Some(member.service_ip),
            vm_ip: Some(member.service_ip),
            vm_prefix: op_cfg.network.vm_prefix,
            vm_gateway: op_cfg.network.vm_gateway,
            vm_dns: op_cfg.network.vm_dns,
            vm_rpc_port: Some(member.rpc_port),
            vm_consensus_port: Some(DEFAULT_CONSENSUS_PORT),
            vm_reth_port: Some(DEFAULT_RETH_PORT),
            vm_discv5_port: Some(DEFAULT_DISCV5_PORT),
            vm_radicle_port: Some(DEFAULT_RADICLE_PORT),
            bootnodes: op_cfg.network.bootnodes,
            use_local_defaults: op_cfg.network.use_local_defaults,
        },
    };
    cfg.validate()?;
    Ok(cfg)
}

fn load_joining_operator_template(build: &NetworkArtifacts) -> Result<OperatorConfig> {
    let example_path = build.root.join(&build.manifest.joining.config_example);
    let raw_example = fs::read_to_string(&example_path).wrap_err_with(|| {
        format!(
            "read joining operator config template at {}",
            example_path.display()
        )
    })?;
    // Parse the template before overriding placeholder endpoint values, then validate strictly
    // after applying the joining member's actual VM network and Hyperlane configuration.
    serde_yaml_ng::from_str(&raw_example).wrap_err_with(|| {
        format!(
            "invalid joining operator config template at {}",
            example_path.display()
        )
    })
}

fn find_crs_file(artifacts: &Path) -> Result<PathBuf> {
    let founder_crs = artifacts.join("vm/validator-0/.bb-crs/bn254_g1.dat");
    if founder_crs.is_file() {
        return Ok(founder_crs);
    }
    bail!(
        "CRS file missing at {} (expected built VM network artifacts)",
        founder_crs.display()
    );
}

fn copy_file_mode(src: &Path, dst: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dst)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn copy_keys_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o700))?;
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(src)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}
