//! VM founder data-tree assembly, bootnode generation, consensus peers, and firewall mappings.

use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use outbe_genesis_cli::launch_bundle;
use outbe_operator_node_role::{
    config::{
        FeederConfig as OperatorFeederConfig, NetworkConfig as OperatorNetworkConfig,
        NetworkPorts as OperatorNetworkPorts, OperatorConfig,
        CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
    validator_address,
};
use serde_json::Value;
use walkdir::WalkDir;

use super::{
    build::{
        copy_tree, required_validator_field, FirewallMappingEntry, FounderArtifact,
        FounderVmArtifact,
    },
    build_joining::joining_feeder_public_toml,
};
use crate::network::config::{
    Founder, FounderPublishedPorts, NetworkConfig, BASE_CONSENSUS_P2P_PORT,
};

pub(super) struct VmTreeInputs<'a> {
    pub(super) staging_root: &'a Path,
    pub(super) software_root: &'a Path,
    pub(super) keys_dir: &'a Path,
    pub(super) config_path: &'a Path,
    pub(super) config: &'a NetworkConfig,
    pub(super) validators: &'a [Value],
    pub(super) bootnodes: &'a [String],
}

pub(super) fn write_vm_bootnodes(
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

pub(crate) fn build_vm_firewall_mappings(
    config: &NetworkConfig,
) -> Result<Vec<FirewallMappingEntry>> {
    if config.vm.is_none() {
        return Ok(Vec::new());
    }
    let mut resolved_founders = Vec::with_capacity(config.founders.len());
    let mut distinct_hosts = BTreeSet::new();
    for founder in &config.founders {
        let resolved = config.resolve_founder_vm(founder)?;
        distinct_hosts.insert(resolved.vmm_host.host.clone());
        resolved_founders.push((founder, resolved));
    }
    let multi_host = distinct_hosts.len() > 1;
    let mut entries = Vec::new();
    for (founder, resolved) in resolved_founders {
        let needs_dnat =
            multi_host || resolved.port_offset > 0 || founder.advertise_ip != resolved.vm_ip;
        if !needs_dnat {
            continue;
        }
        let ports = FounderPublishedPorts::from_offset(resolved.port_offset)?;
        for (public_port, vm_port, proto, purpose) in ports.dnat_ports() {
            entries.push(FirewallMappingEntry {
                member: founder.name.clone(),
                host: resolved.vmm_host.host.clone(),
                public_ip: founder.advertise_ip,
                public_port,
                proto: proto.to_owned(),
                vm_ip: resolved.vm_ip,
                vm_port,
                purpose: purpose.to_owned(),
            });
        }
    }
    Ok(entries)
}

pub(crate) fn render_firewall_mappings_table(entries: &[FirewallMappingEntry]) -> String {
    let mut out = format!(
        "  {:<14} {:<16} {:<16} {:<11} {:<5} {:<14} {:<7} {}\n",
        "MEMBER", "HOST", "PUBLIC_IP", "PUBLIC_PORT", "PROTO", "VM_IP", "VM_PORT", "PURPOSE"
    );
    for entry in entries {
        out.push_str(&format!(
            "  {:<14} {:<16} {:<16} {:<11} {:<5} {:<14} {:<7} {}\n",
            entry.member,
            entry.host,
            entry.public_ip,
            entry.public_port,
            entry.proto,
            entry.vm_ip,
            entry.vm_port,
            entry.purpose
        ));
    }
    out.trim_end().to_owned()
}

pub(super) fn emit_vm_founder_trees(inputs: &VmTreeInputs<'_>) -> Result<Vec<FounderArtifact>> {
    let peers = consensus_peers(inputs.config, inputs.validators)?;
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

    inputs
        .config
        .founders
        .iter()
        .zip(inputs.validators)
        .enumerate()
        .map(|(index, (founder, validator))| {
            emit_single_vm_founder_tree(
                inputs,
                &FounderTreeContext {
                    index,
                    founder,
                    validator,
                    peers: &peers,
                    feeder_public_toml: &feeder_public_toml,
                    crs_source: &crs_source,
                    genesis_source: &genesis_source,
                },
            )
        })
        .collect()
}

struct FounderTreeContext<'a> {
    index: usize,
    founder: &'a Founder,
    validator: &'a Value,
    peers: &'a [String],
    feeder_public_toml: &'a str,
    crs_source: &'a Path,
    genesis_source: &'a Path,
}

fn emit_single_vm_founder_tree(
    inputs: &VmTreeInputs<'_>,
    ctx: &FounderTreeContext<'_>,
) -> Result<FounderArtifact> {
    let resolved_vm = inputs.config.resolve_founder_vm(ctx.founder)?;
    let relative = format!("vm/validator-{}", ctx.index);
    let tree = inputs.staging_root.join(&relative);
    fs::create_dir_all(&tree)?;
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o755))?;

    copy_file_mode(ctx.genesis_source, &tree.join("genesis.json"), 0o644)?;
    copy_file_mode(ctx.crs_source, &tree.join(".bb-crs/bn254_g1.dat"), 0o644)?;

    let src_keys = inputs.keys_dir.join(format!("validator-{}", ctx.index));
    let dst_keys = tree.join("keys");
    copy_vm_keys(&src_keys, &dst_keys)?;

    let validator_addr = validator_address(&dst_keys.join("evm-key.hex"))?;
    let ports = FounderPublishedPorts::from_offset(resolved_vm.port_offset)?;
    let rpc_bind_ip = inputs.config.founder_rpc_bind_ip(ctx.founder)?;
    ensure!(
        rpc_bind_ip == resolved_vm.vm_ip,
        "founder `{}` sets rpc_bind_ip={rpc_bind_ip} != vm.vm_ip={}; \
         outbe-net-up requires rpc_bind_ip to match vm_ip for RPC-based readiness checks \
         (omit rpc_bind_ip or set it to {})",
        ctx.founder.name,
        resolved_vm.vm_ip,
        resolved_vm.vm_ip
    );
    let operator = OperatorConfig {
        version: OPERATOR_CONFIG_VERSION,
        network: OperatorNetworkConfig {
            chain_id: inputs.config.chain_id,
            certified_rpc: inputs.config.certified_rpc.clone(),
            advertise_ip: ctx.founder.advertise_ip,
            rpc_bind_ip: Some(rpc_bind_ip),
            ports: OperatorNetworkPorts {
                rpc: ctx.founder.rpc_port(),
                ..OperatorNetworkPorts::default()
            },
            vm_ip: Some(resolved_vm.vm_ip),
            vm_prefix: Some(resolved_vm.prefix),
            vm_gateway: Some(resolved_vm.gateway),
            vm_dns: resolved_vm.dns.clone(),
            vm_rpc_port: Some(ctx.founder.rpc_port()),
            vm_consensus_port: Some(ports.consensus_p2p_tcp),
            vm_reth_port: Some(ports.reth_p2p_tcp),
            vm_discv5_port: Some(ports.reth_discv5_udp),
            vm_radicle_port: Some(ports.radicle_tcp),
            bootnodes: inputs.bootnodes.to_vec(),
            consensus_peers: ctx.peers.to_vec(),
            use_local_defaults: inputs.config.topology.use_local_defaults,
        },
        feeder: OperatorFeederConfig {
            public_toml: ctx.feeder_public_toml.to_owned(),
        },
        hyperlane: inputs.config.resolve_hyperlane_config(ctx.founder)?,
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

    let has_authorized_keys = stage_authorized_keys(
        inputs.config_path,
        &tree,
        &ctx.founder.name,
        resolved_vm.authorized_keys.as_deref(),
    )?;

    Ok(FounderArtifact {
        index: ctx.index,
        name: ctx.founder.name.clone(),
        ssh_host: None,
        advertise_ip: ctx.founder.advertise_ip,
        rpc_port: ctx.founder.rpc_port(),
        validator_address: required_validator_field(ctx.validator, "address")?.to_owned(),
        hyperlane_address: required_validator_field(ctx.validator, "hyperlane_address")?.to_owned(),
        consensus_public_key: required_validator_field(ctx.validator, "public_key")?.to_owned(),
        archive: None,
        archive_sha256: None,
        vm: Some(FounderVmArtifact {
            data_tree: relative,
            vmm_host: resolved_vm.vmm_host.host,
            vmm_user: resolved_vm.vmm_host.user,
            vmm_port: resolved_vm.vmm_host.port,
            vm_ip: resolved_vm.vm_ip,
            port_offset: resolved_vm.port_offset,
            vcpu: resolved_vm.vcpu,
            memory_gib: resolved_vm.memory_gib,
            disk_gib: resolved_vm.disk_gib,
            flavor: resolved_vm.flavor,
            has_authorized_keys,
        }),
    })
}

fn stage_authorized_keys(
    config_path: &Path,
    tree: &Path,
    founder_name: &str,
    configured: Option<&Path>,
) -> Result<bool> {
    let Some(configured) = configured else {
        return Ok(false);
    };
    let source = resolve_relative_path(config_path, configured);
    ensure!(
        source.is_file(),
        "vm.authorized_keys does not exist for {founder_name}: {}",
        source.display()
    );
    let dev_dir = tree.join("dev");
    fs::create_dir_all(&dev_dir)?;
    fs::set_permissions(&dev_dir, fs::Permissions::from_mode(0o700))?;
    copy_file_mode(&source, &dev_dir.join("authorized_keys"), 0o600)?;
    Ok(true)
}

pub(super) fn consensus_peers(config: &NetworkConfig, validators: &[Value]) -> Result<Vec<String>> {
    config
        .founders
        .iter()
        .zip(validators)
        .map(|(founder, validator)| format_consensus_peer(config, founder, validator))
        .collect()
}

fn format_consensus_peer(
    config: &NetworkConfig,
    founder: &Founder,
    validator: &Value,
) -> Result<String> {
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
    let expected_offset_port = BASE_CONSENSUS_P2P_PORT
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
        let mode = if entry.file_type().is_dir() {
            0o700
        } else {
            0o600
        };
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode))?;
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
