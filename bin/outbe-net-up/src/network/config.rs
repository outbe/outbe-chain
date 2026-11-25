use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::FromStr,
};

use alloy_primitives::{Address, U256};
use eyre::{ensure, Context, Result};
use outbe_operator_node_role::{
    config::{HyperlaneConfig, HyperlaneOrigin, ReorgPeriod, RpcConsensus},
    progress::Progress,
};
use outbe_operator_remote::validate_ssh;
pub use outbe_operator_remote::Ssh;
use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;
pub const MIN_FOUNDERS: usize = 4;
pub const EXAMPLE: &str = include_str!("../../templates/network/network.example.yaml");

pub const BASE_RPC_PORT: u16 = 8545;
pub const BASE_RADICLE_PORT: u16 = 8776;
pub const BASE_HYPERLANE_PORT: u16 = 9000;
pub const BASE_RETH_P2P_PORT: u16 = 30303;
pub const BASE_CONSENSUS_P2P_PORT: u16 = 30400;
pub const BASE_RETH_DISCV5_PORT: u16 = 31303;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub version: u32,
    pub name: String,
    pub network: Network,
    pub chain_id: u64,
    pub certified_rpc: String,
    pub founders: Vec<Founder>,
    pub hyperlane: HyperlaneNetworkConfig,
    #[serde(default)]
    pub topology: Topology,
    pub genesis: Genesis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<VmNetworkConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VmNetworkConfig {
    /// Default VMM host SSH endpoint shared by founders that do not override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vmm_host: Option<Ssh>,
    /// VMM home directory on the host (contains `run/vmm.sock`, `image/`, `vm/`).
    #[serde(default = "default_vm_vmm_home")]
    pub vmm_home: String,
    /// Host Linux bridge name attached to VM NICs.
    #[serde(default = "default_vm_bridge")]
    pub bridge: String,
    /// IPv4 CIDR prefix length for VM static addressing (required, 1..=30, e.g. `24`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<u8>,
    /// Default IPv4 gateway for VM static addressing (required, e.g. `10.77.0.1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Ipv4Addr>,
    /// IPv4 DNS servers for VM static addressing (required non-empty, e.g. `[1.1.1.1, 8.8.8.8]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<Ipv4Addr>,
    /// Legacy optional dnsmasq DHCP hosts file override (hosts now auto-discover via `outbe-up host reserve`).
    #[serde(default = "default_vm_dhcp_hosts_file")]
    pub dhcp_hosts_file: PathBuf,
    /// Legacy optional dnsmasq DHCP leases file override.
    #[serde(default = "default_vm_dhcp_leases_file")]
    pub dhcp_leases_file: PathBuf,
    /// Legacy optional dnsmasq PID file override.
    #[serde(default = "default_vm_dhcp_pid_file")]
    pub dhcp_pid_file: PathBuf,
    #[serde(default = "default_vm_vcpu")]
    pub vcpu: u32,
    #[serde(default = "default_vm_memory_gib")]
    pub memory_gib: u32,
    #[serde(default = "default_vm_disk_gib")]
    pub disk_gib: u32,
    #[serde(default)]
    pub flavor: VmFlavor,
    /// Optional path to an `authorized_keys` file for `dev` VMs, relative to `network.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_keys: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmFlavor {
    #[default]
    Dev,
    Prod,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FounderVmConfig {
    /// Private bridge IPv4 address assigned to this founder VM.
    pub bridge_ip: Ipv4Addr,
    /// Optional host port offset added to base service ports (`rpc`, `radicle`, `hyperlane`,
    /// `reth_p2p`, `consensus_p2p`, `reth_discv5`) for multi-host / published VM deployments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_offset: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vmm_host: Option<Ssh>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vmm_home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_gib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flavor: Option<VmFlavor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_keys: Option<PathBuf>,
}

/// Computed host publish ports for one founder VM given its `port_offset`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct FounderPublishedPorts {
    pub rpc_tcp: u16,
    pub radicle_tcp: u16,
    pub hyperlane_tcp: u16,
    pub reth_p2p_tcp: u16,
    pub reth_p2p_udp: u16,
    pub consensus_p2p_tcp: u16,
    pub reth_discv5_udp: u16,
}

impl FounderPublishedPorts {
    pub fn from_offset(offset: u16) -> Result<Self> {
        let add = |base: u16, label: &str| -> Result<u16> {
            base.checked_add(offset).ok_or_else(|| {
                eyre::eyre!(
                    "port_offset {offset} overflows u16 port range for {label} (base {base})"
                )
            })
        };
        Ok(Self {
            rpc_tcp: add(BASE_RPC_PORT, "rpc")?,
            radicle_tcp: add(BASE_RADICLE_PORT, "radicle")?,
            hyperlane_tcp: add(BASE_HYPERLANE_PORT, "hyperlane")?,
            reth_p2p_tcp: add(BASE_RETH_P2P_PORT, "reth_p2p/tcp")?,
            reth_p2p_udp: add(BASE_RETH_P2P_PORT, "reth_p2p/udp")?,
            consensus_p2p_tcp: add(BASE_CONSENSUS_P2P_PORT, "consensus_p2p")?,
            reth_discv5_udp: add(BASE_RETH_DISCV5_PORT, "reth_discv5/udp")?,
        })
    }

    /// Return all `(service_label, proto, host_port)` bindings for collision checking.
    pub fn bindings(self) -> [(&'static str, &'static str, u16); 7] {
        [
            ("rpc", "tcp", self.rpc_tcp),
            ("radicle", "tcp", self.radicle_tcp),
            ("hyperlane", "tcp", self.hyperlane_tcp),
            ("reth_p2p", "tcp", self.reth_p2p_tcp),
            ("reth_p2p", "udp", self.reth_p2p_udp),
            ("consensus_p2p", "tcp", self.consensus_p2p_tcp),
            ("reth_discv5", "udp", self.reth_discv5_udp),
        ]
    }

    /// Return `outbe-up host check|publish --port` specs for 1:1 P2P/Radicle DNAT
    /// (`host_port == guest_port == base + offset`) plus Hyperlane (`9000 -> 9000 + offset`).
    #[allow(dead_code)]
    pub fn publish_port_specs(self) -> Vec<String> {
        let hyperlane = if self.hyperlane_tcp == BASE_HYPERLANE_PORT {
            format!("{BASE_HYPERLANE_PORT}/tcp")
        } else {
            format!("{BASE_HYPERLANE_PORT}:{}/tcp", self.hyperlane_tcp)
        };
        vec![
            format!("{}/tcp", self.reth_p2p_tcp),
            format!("{}/udp", self.reth_p2p_udp),
            format!("{}/tcp", self.consensus_p2p_tcp),
            format!("{}/udp", self.reth_discv5_udp),
            format!("{}/tcp", self.radicle_tcp),
            hyperlane,
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFounderVm {
    pub vmm_host: Ssh,
    pub vmm_home: String,
    pub bridge: String,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    #[allow(dead_code)]
    pub dhcp_hosts_file: PathBuf,
    #[allow(dead_code)]
    pub dhcp_leases_file: PathBuf,
    #[allow(dead_code)]
    pub dhcp_pid_file: PathBuf,
    pub bridge_ip: Ipv4Addr,
    pub port_offset: u16,
    pub port_offset_explicit: bool,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub flavor: VmFlavor,
    pub authorized_keys: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Network {
    Devnet,
    Testnet,
    Mainnet,
}

impl Network {
    pub const fn chain_id(self) -> u64 {
        match self {
            Self::Devnet => 424_242,
            Self::Testnet => 54_322_345,
            Self::Mainnet => 676,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Devnet => "devnet",
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Founder {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Ssh>,
    pub advertise_ip: Ipv4Addr,
    #[serde(default = "loopback")]
    pub rpc_bind_ip: Ipv4Addr,
    pub hyperlane_public_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<FounderVmConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlaneNetworkConfig {
    #[serde(default = "default_hyperlane_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub gasless: bool,
    #[serde(default = "default_hyperlane_funding_wei")]
    pub signer_funding_wei: String,
    #[serde(default)]
    pub origins: Vec<HyperlaneOrigin>,
    /// Optional local EVM origin (e.g. host-side anvil) for VM networks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_evm: Option<LocalEvmOriginConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalEvmOriginConfig {
    /// Hyperlane domain ID (non-zero, distinct from `chain_id`).
    pub domain: u32,
    /// HTTP(S) RPC URL reachable from inside the guest VM (e.g. `http://10.77.0.1:8546`).
    pub rpc_url: String,
    /// Confirmation lag in blocks (defaults to 0 for deterministic local mining).
    #[serde(default)]
    pub reorg_period: u64,
    /// Polling interval in seconds (defaults to 15).
    #[serde(default = "default_local_evm_interval")]
    pub interval_secs: u64,
}

const fn default_local_evm_interval() -> u64 {
    15
}

impl HyperlaneNetworkConfig {
    pub fn for_public_url(&self, public_url: String) -> HyperlaneConfig {
        HyperlaneConfig {
            public_url,
            poll_interval_secs: self.poll_interval_secs,
            gasless: self.gasless,
            signer_funding_wei: self.signer_funding_wei.clone(),
            origins: self.origins.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Topology {
    #[serde(default)]
    pub use_local_defaults: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Genesis {
    /// Optional pre-TEE genesis JSON used instead of the generated base genesis.
    #[serde(default)]
    pub base: Option<PathBuf>,
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub epoch_length_blocks: Option<u64>,
    #[serde(default)]
    pub dkg_prepare_window_blocks: Option<u64>,
    #[serde(default)]
    pub dkg_activation_grace_blocks: Option<u64>,
    #[serde(default)]
    pub prefunded_accounts: BTreeMap<String, String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, serde_yaml_ng::Value>,
}

impl NetworkConfig {
    pub fn read(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .wrap_err_with(|| format!("read network config: {}", path.display()))?;
        let config: Self = serde_yaml_ng::from_str(&raw)
            .wrap_err_with(|| format!("invalid network config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == CONFIG_VERSION,
            "unsupported network config version {}",
            self.version
        );
        ensure!(
            safe_name(&self.name),
            "name must use letters, digits, `-`, or `_`"
        );
        match self.network {
            Network::Mainnet => {
                ensure!(
                    self.chain_id == Network::Mainnet.chain_id(),
                    "mainnet requires chain_id {}",
                    Network::Mainnet.chain_id()
                );
            }
            Network::Devnet | Network::Testnet => {
                ensure!(
                    self.chain_id > 0 && self.chain_id != Network::Mainnet.chain_id(),
                    "{} requires a non-zero chain_id different from mainnet ({})",
                    self.network.as_str(),
                    Network::Mainnet.chain_id()
                );
            }
        }
        ensure!(
            (self.certified_rpc.starts_with("http://")
                || self.certified_rpc.starts_with("https://"))
                && !self.certified_rpc.contains(char::is_whitespace),
            "certified_rpc must be an HTTP(S) URL without whitespace"
        );
        ensure!(
            self.founders.len() >= MIN_FOUNDERS,
            "at least {MIN_FOUNDERS} founding validators are required"
        );

        if let Some(vm) = &self.vm {
            ensure!(
                self.network != Network::Mainnet,
                "vm deployment is not supported on mainnet"
            );
            ensure!(
                !vm.vmm_home.trim().is_empty(),
                "vm.vmm_home must not be empty"
            );
            ensure!(
                safe_name(&vm.bridge),
                "vm.bridge must use letters, digits, `-`, or `_`"
            );
            ensure!(
                vm.prefix.is_some_and(|p| (1..=30).contains(&p)),
                "vm.prefix is required and must be an IPv4 prefix length in 1..=30"
            );
            ensure!(
                vm.gateway.is_some(),
                "vm.gateway is required for VM static addressing"
            );
            ensure!(
                !vm.dns.is_empty(),
                "vm.dns is required and must list at least one IPv4 DNS server"
            );
            ensure!(
                vm.dhcp_hosts_file.is_absolute(),
                "vm.dhcp_hosts_file must be an absolute path"
            );
            ensure!(
                vm.dhcp_leases_file.is_absolute(),
                "vm.dhcp_leases_file must be an absolute path"
            );
            ensure!(
                vm.dhcp_pid_file.is_absolute(),
                "vm.dhcp_pid_file must be an absolute path"
            );
            ensure!(vm.vcpu > 0, "vm.vcpu must be positive");
            ensure!(vm.memory_gib > 0, "vm.memory_gib must be positive");
            ensure!(vm.disk_gib > 0, "vm.disk_gib must be positive");
            if let Some(ssh) = &vm.vmm_host {
                validate_ssh(ssh, "vm.vmm_host")?;
            }
        }

        let mut names = BTreeSet::new();
        let mut ssh_targets = BTreeSet::new();
        let mut advertise_ips = BTreeSet::new();
        let mut vm_resolved: Vec<(&Founder, ResolvedFounderVm)> = Vec::new();
        let mut bridge_ips_per_host: BTreeSet<(String, Ipv4Addr)> = BTreeSet::new();
        let mut distinct_vmm_hosts: BTreeSet<String> = BTreeSet::new();
        let mut hosts_with_explicit_offset: BTreeSet<String> = BTreeSet::new();

        for founder in &self.founders {
            ensure!(
                safe_name(&founder.name),
                "invalid founder name: {}",
                founder.name
            );
            ensure!(
                names.insert(&founder.name),
                "duplicate founder name: {}",
                founder.name
            );
            if self.vm.is_some() {
                ensure!(
                    founder.ssh.is_none(),
                    "founder {} may not set ssh when top-level vm deployment is enabled",
                    founder.name
                );
                let resolved = self.resolve_founder_vm(founder)?;
                validate_ssh(&resolved.vmm_host, &founder.name)?;
                ensure!(
                    resolved.vcpu > 0,
                    "vm.vcpu must be positive for {}",
                    founder.name
                );
                ensure!(
                    resolved.memory_gib > 0,
                    "vm.memory_gib must be positive for {}",
                    founder.name
                );
                ensure!(
                    resolved.disk_gib > 0,
                    "vm.disk_gib must be positive for {}",
                    founder.name
                );
                ensure!(
                    resolved.bridge_ip.is_private(),
                    "vm.bridge_ip for {} must be a private IPv4 address",
                    founder.name
                );
                ensure!(
                    founder.rpc_bind_ip == resolved.bridge_ip,
                    "founder {} rpc_bind_ip ({}) must equal vm.bridge_ip ({}) for static VM addressing",
                    founder.name,
                    founder.rpc_bind_ip,
                    resolved.bridge_ip
                );
                outbe_operator_node_role::vm_network::StaticVmNetworkConfig {
                    address: resolved.bridge_ip,
                    prefix_len: resolved.prefix,
                    gateway: resolved.gateway,
                    dns: resolved.dns.clone(),
                }
                .validate()
                .wrap_err_with(|| format!("invalid static VM addressing for {}", founder.name))?;
                ensure!(
                    bridge_ips_per_host
                        .insert((resolved.vmm_host.host.clone(), resolved.bridge_ip)),
                    "duplicate vm.bridge_ip {} on host {}",
                    resolved.bridge_ip,
                    resolved.vmm_host.host
                );
                if resolved.flavor == VmFlavor::Prod {
                    ensure!(
                        resolved.authorized_keys.is_none(),
                        "prod VM flavor does not allow authorized_keys for {}",
                        founder.name
                    );
                }
                // Validate port_offset arithmetic for every VM founder up front.
                let _ = FounderPublishedPorts::from_offset(resolved.port_offset)
                    .wrap_err_with(|| format!("invalid vm.port_offset for {}", founder.name))?;
                distinct_vmm_hosts.insert(resolved.vmm_host.host.clone());
                if resolved.port_offset_explicit {
                    hosts_with_explicit_offset.insert(resolved.vmm_host.host.clone());
                }
                vm_resolved.push((founder, resolved));
            } else {
                ensure!(
                    founder.vm.is_none(),
                    "founder {} sets vm without a top-level vm section",
                    founder.name
                );
                let ssh = founder
                    .ssh
                    .as_ref()
                    .ok_or_else(|| eyre::eyre!("missing ssh configuration for {}", founder.name))?;
                validate_ssh(ssh, &founder.name)?;
                ensure!(
                    ssh_targets.insert((&ssh.host, ssh.port)),
                    "duplicate SSH target: {}:{}",
                    ssh.host,
                    ssh.port
                );
                ensure!(
                    advertise_ips.insert(founder.advertise_ip),
                    "duplicate advertise_ip: {}",
                    founder.advertise_ip
                );
            }
            ensure!(
                founder.rpc_bind_ip.is_loopback() || founder.rpc_bind_ip.is_private(),
                "rpc_bind_ip for {} must be loopback or private",
                founder.name
            );
            self.resolve_hyperlane_config(founder)
                .wrap_err_with(|| format!("invalid Hyperlane config for {}", founder.name))?;
        }

        // Validate VM per-host & per-advertise_ip computed port uniqueness:
        // - Always enforced on multi-host VM deployments (where cross-host publishing is required)
        //   AND on any single host where at least one founder sets `vm.port_offset`.
        // - Also enforce that no two VM founders share the exact same `(advertise_ip, proto, port)`
        //   endpoint (catches duplicate advertise_ip without distinct port_offset, as well as
        //   cross-service collisions like port_offset 97 where 30303 + 97 == 30400).
        if !vm_resolved.is_empty() {
            let is_multi_host = distinct_vmm_hosts.len() > 1;
            let mut host_ports: BTreeMap<(String, &'static str, u16), (String, &'static str)> =
                BTreeMap::new();
            let mut adv_ports: BTreeMap<(Ipv4Addr, &'static str, u16), (String, &'static str)> =
                BTreeMap::new();

            for (founder, resolved) in &vm_resolved {
                let ports = FounderPublishedPorts::from_offset(resolved.port_offset)?;
                let enforce_host_ports =
                    is_multi_host || hosts_with_explicit_offset.contains(&resolved.vmm_host.host);
                for (service, proto, port) in ports.bindings() {
                    if enforce_host_ports {
                        if let Some((prev_founder, prev_service)) = host_ports.insert(
                            (resolved.vmm_host.host.clone(), proto, port),
                            (founder.name.clone(), service),
                        ) {
                            eyre::bail!(
                                "computed host port collision on `{}`: {} ({service}/{proto}:{port}) collides with {} ({prev_service}/{proto}:{port}); assign distinct `vm.port_offset` values per host",
                                resolved.vmm_host.host,
                                founder.name,
                                prev_founder
                            );
                        }
                    }
                    if let Some((prev_founder, prev_service)) = adv_ports.insert(
                        (founder.advertise_ip, proto, port),
                        (founder.name.clone(), service),
                    ) {
                        eyre::bail!(
                            "computed advertised endpoint collision on `{}`: {} ({service}/{proto}:{port}) collides with {} ({prev_service}/{proto}:{port}); assign distinct `advertise_ip` or `vm.port_offset` values",
                            founder.advertise_ip,
                            founder.name,
                            prev_founder
                        );
                    }
                }
            }
        }

        for (address, amount) in &self.genesis.prefunded_accounts {
            Address::from_str(address)
                .wrap_err_with(|| format!("invalid prefunded account address: {address}"))?;
            let amount = U256::from_str(amount)
                .wrap_err_with(|| format!("invalid prefunded amount for {address}"))?;
            ensure!(
                !amount.is_zero(),
                "prefunded amount must be positive for {address}"
            );
        }
        for (name, value) in [
            ("epoch_length_blocks", self.genesis.epoch_length_blocks),
            (
                "dkg_prepare_window_blocks",
                self.genesis.dkg_prepare_window_blocks,
            ),
            (
                "dkg_activation_grace_blocks",
                self.genesis.dkg_activation_grace_blocks,
            ),
        ] {
            if let Some(value) = value {
                ensure!(value > 0, "genesis.{name} must be non-zero");
            }
        }
        if self.genesis.base.is_some() {
            ensure!(
                self.genesis.timestamp.is_none()
                    && self.genesis.epoch_length_blocks.is_none()
                    && self.genesis.dkg_prepare_window_blocks.is_none()
                    && self.genesis.dkg_activation_grace_blocks.is_none(),
                "genesis timestamp, epoch, and DKG parameters must come from genesis.base when it is set"
            );
        }
        let reserved = reserved_genesis_keys();
        if let Some(key) = self
            .genesis
            .overrides
            .keys()
            .find(|key| reserved.contains(key.as_str()))
        {
            eyre::bail!("genesis.overrides may not replace managed key `{key}`");
        }

        Ok(())
    }

    /// Resolve effective VM settings for one founder by merging overrides over network defaults.
    pub fn resolve_founder_vm(&self, founder: &Founder) -> Result<ResolvedFounderVm> {
        let defaults = self
            .vm
            .as_ref()
            .ok_or_else(|| eyre::eyre!("network config has no vm section"))?;
        let override_cfg = founder
            .vm
            .as_ref()
            .ok_or_else(|| eyre::eyre!("missing vm settings for founder {}", founder.name))?;
        let vmm_host = override_cfg
            .vmm_host
            .clone()
            .or_else(|| defaults.vmm_host.clone())
            .ok_or_else(|| eyre::eyre!("missing vm.vmm_host for founder {}", founder.name))?;
        let prefix = defaults
            .prefix
            .ok_or_else(|| eyre::eyre!("missing vm.prefix in network config"))?;
        let gateway = defaults
            .gateway
            .ok_or_else(|| eyre::eyre!("missing vm.gateway in network config"))?;
        ensure!(!defaults.dns.is_empty(), "missing vm.dns in network config");
        Ok(ResolvedFounderVm {
            vmm_host,
            vmm_home: override_cfg
                .vmm_home
                .clone()
                .unwrap_or_else(|| defaults.vmm_home.clone()),
            bridge: override_cfg
                .bridge
                .clone()
                .unwrap_or_else(|| defaults.bridge.clone()),
            prefix,
            gateway,
            dns: defaults.dns.clone(),
            dhcp_hosts_file: defaults.dhcp_hosts_file.clone(),
            dhcp_leases_file: defaults.dhcp_leases_file.clone(),
            dhcp_pid_file: defaults.dhcp_pid_file.clone(),
            bridge_ip: override_cfg.bridge_ip,
            port_offset: override_cfg.port_offset.unwrap_or(0),
            port_offset_explicit: override_cfg.port_offset.is_some(),
            vcpu: override_cfg.vcpu.unwrap_or(defaults.vcpu),
            memory_gib: override_cfg.memory_gib.unwrap_or(defaults.memory_gib),
            disk_gib: override_cfg.disk_gib.unwrap_or(defaults.disk_gib),
            flavor: override_cfg.flavor.unwrap_or(defaults.flavor),
            authorized_keys: override_cfg
                .authorized_keys
                .clone()
                .or_else(|| defaults.authorized_keys.clone()),
        })
    }

    /// Resolve effective Hyperlane validator configuration for a founder.
    pub fn resolve_hyperlane_config(&self, founder: &Founder) -> Result<HyperlaneConfig> {
        self.resolve_hyperlane_for_endpoint(
            founder.hyperlane_public_url.clone(),
            founder.rpc_bind_ip,
        )
    }

    /// Resolve Hyperlane configuration for the joining operator example config.
    pub fn resolve_joining_hyperlane_config(&self) -> Result<HyperlaneConfig> {
        self.resolve_hyperlane_for_endpoint("http://192.0.2.10:9000".into(), Ipv4Addr::LOCALHOST)
    }

    fn resolve_hyperlane_for_endpoint(
        &self,
        public_url: String,
        local_rpc_ip: Ipv4Addr,
    ) -> Result<HyperlaneConfig> {
        if self.vm.is_none() {
            ensure!(
                self.hyperlane.local_evm.is_none(),
                "hyperlane.local_evm is only supported for VM deployments"
            );
            let cfg = self.hyperlane.for_public_url(public_url);
            cfg.validate()?;
            return Ok(cfg);
        }

        let local_domain = u32::try_from(self.chain_id).wrap_err_with(|| {
            format!(
                "chain_id {} does not fit in Hyperlane uint32 domain",
                self.chain_id
            )
        })?;
        ensure!(local_domain > 0, "chain_id must be non-zero");

        let origins = if self.hyperlane.origins.is_empty() {
            let mut generated = vec![HyperlaneOrigin {
                domain: local_domain,
                rpc_urls: vec![format!("http://{local_rpc_ip}:8545")],
                rpc_consensus: RpcConsensus::Single,
                reorg_period: ReorgPeriod::Blocks(0),
                interval_secs: default_local_evm_interval(),
                ws_url: None,
                index_from: None,
            }];
            if let Some(local_evm) = &self.hyperlane.local_evm {
                ensure!(
                    local_evm.domain > 0,
                    "hyperlane.local_evm.domain must be non-zero"
                );
                ensure!(
                    local_evm.domain != local_domain,
                    "hyperlane.local_evm.domain ({}) must differ from local chain_id ({local_domain})",
                    local_evm.domain
                );
                generated.push(HyperlaneOrigin {
                    domain: local_evm.domain,
                    rpc_urls: vec![local_evm.rpc_url.clone()],
                    rpc_consensus: RpcConsensus::Single,
                    reorg_period: ReorgPeriod::Blocks(local_evm.reorg_period),
                    interval_secs: local_evm.interval_secs,
                    ws_url: None,
                    index_from: None,
                });
            }
            generated
        } else {
            ensure!(
                self.hyperlane.local_evm.is_none(),
                "hyperlane.origins must be empty when hyperlane.local_evm is configured on a VM network"
            );
            ensure!(
                self.hyperlane
                    .origins
                    .iter()
                    .any(|origin| origin.domain == local_domain),
                "VM hyperlane.origins must include local domain {local_domain} (or omit origins to auto-generate local origins)"
            );
            self.hyperlane.origins.clone()
        };

        let cfg = HyperlaneConfig {
            public_url,
            poll_interval_secs: self.hyperlane.poll_interval_secs,
            gasless: self.hyperlane.gasless,
            signer_funding_wei: self.hyperlane.signer_funding_wei.clone(),
            origins,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

pub fn init(path: &Path) -> Result<()> {
    let mut progress = Progress::new();
    progress.stage("writing network configuration example");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create network config: {}", path.display()))?;
    file.write_all(EXAMPLE.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    NetworkConfig::read(path)?;
    progress.done(&format!(
        "wrote network configuration to {}",
        path.display()
    ));
    Ok(())
}

fn reserved_genesis_keys() -> BTreeSet<&'static str> {
    [
        "network",
        "chain_id",
        "validators",
        "keys_dir",
        "tee",
        "base_genesis",
        "timestamp",
        "epoch_length_blocks",
        "dkg_prepare_window_blocks",
        "dkg_activation_grace_blocks",
        "balance",
        "output",
        "chain_binary",
        "keygen_binary",
        "node_binary",
        "radicle_binary",
        "feeder_binary",
        "remote_base_dir",
        "remote_keys_dir",
        "rpc_bind_ip",
        "consensus_use_local_defaults",
    ]
    .into_iter()
    .collect()
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

const fn loopback() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

const fn default_hyperlane_poll_interval() -> u64 {
    30
}

fn default_hyperlane_funding_wei() -> String {
    "1000000000000000000".to_owned()
}

fn default_vm_vmm_home() -> String {
    "~/.dstack-vmm".to_owned()
}

fn default_vm_bridge() -> String {
    "obr0".to_owned()
}

fn default_vm_dhcp_hosts_file() -> PathBuf {
    PathBuf::from("/var/lib/validator-vm/dhcp-hosts")
}

fn default_vm_dhcp_leases_file() -> PathBuf {
    PathBuf::from("/var/lib/validator-vm/dhcp-leases")
}

fn default_vm_dhcp_pid_file() -> PathBuf {
    PathBuf::from("/run/validator-vm-dhcp.pid")
}

const fn default_vm_vcpu() -> u32 {
    3
}

const fn default_vm_memory_gib() -> u32 {
    5
}

const fn default_vm_disk_gib() -> u32 {
    40
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(founder_count: usize) -> NetworkConfig {
        serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: local-testnet
network: testnet
chain_id: 54322345
certified_rpc: "http://10.0.0.10:8545"
founders:
{}
hyperlane:
  origins:
    - domain: 54322345
      rpc_urls: ["https://rpc.example.invalid"]
      reorg_period: 0
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts:
    "0x1111111111111111111111111111111111111111": "1000000000000000000"
"#,
            (0..founder_count)
                .map(|index| format!(
                    "  - name: validator-{index}\n    ssh:\n      host: 192.0.2.{}\n    advertise_ip: 10.0.0.{}\n    hyperlane_public_url: http://192.0.2.{}:9000",
                    index + 10,
                    index + 10,
                    index + 10
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
        .unwrap()
    }

    #[test]
    fn accepts_four_or_more_founders() {
        for count in [4, 5, 7] {
            config_with(count).validate().unwrap();
        }
    }

    #[test]
    fn rejects_too_few_founders() {
        assert!(config_with(3)
            .validate()
            .unwrap_err()
            .to_string()
            .contains("at least 4"));
    }

    #[test]
    fn example_is_valid() {
        let config: NetworkConfig = serde_yaml_ng::from_str(EXAMPLE).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn testnet_allows_custom_non_mainnet_chain_id_while_mainnet_stays_pinned() {
        // Testnet accepts custom non-676 chain_id (e.g. 54322346 or 424242).
        let mut custom_testnet = config_with(4);
        custom_testnet.chain_id = 54_322_346;
        custom_testnet.hyperlane.origins[0].domain = 54_322_346;
        custom_testnet.validate().unwrap();

        // Testnet MUST refuse mainnet chain_id 676 and chain_id 0.
        let mut testnet_676 = config_with(4);
        testnet_676.chain_id = 676;
        assert!(testnet_676.validate().is_err());
        testnet_676.chain_id = 0;
        assert!(testnet_676.validate().is_err());

        // Mainnet stays strictly pinned to 676.
        let mut mainnet = config_with(4);
        mainnet.network = Network::Mainnet;
        mainnet.chain_id = 676;
        mainnet.validate().unwrap();
        mainnet.chain_id = 677;
        assert!(mainnet.validate().is_err());
    }

    fn mixed_host_vm_config(offsets: [Option<u16>; 4]) -> NetworkConfig {
        let fmt_offset = |opt: Option<u16>| {
            opt.map_or_else(String::new, |v| format!("\n      port_offset: {v}"))
        };
        serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: mixed-vm-testnet
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
      bridge_ip: 10.77.0.10{o0}
  - name: validator-1
    advertise_ip: 162.19.107.121
    rpc_bind_ip: 10.77.0.11
    hyperlane_public_url: http://162.19.107.121:9001
    vm:
      bridge_ip: 10.77.0.11{o1}
  - name: validator-2
    advertise_ip: 141.95.168.110
    rpc_bind_ip: 10.77.0.10
    hyperlane_public_url: http://141.95.168.110:9000
    vm:
      bridge_ip: 10.77.0.10{o2}
      vmm_host:
        host: 141.95.168.110
  - name: validator-3
    advertise_ip: 141.95.168.110
    rpc_bind_ip: 10.77.0.11
    hyperlane_public_url: http://141.95.168.110:9001
    vm:
      bridge_ip: 10.77.0.11{o3}
      vmm_host:
        host: 141.95.168.110
hyperlane:
  origins:
    - domain: 54322345
      rpc_urls: ["https://rpc.example.invalid"]
      reorg_period: 0
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#,
            o0 = fmt_offset(offsets[0]),
            o1 = fmt_offset(offsets[1]),
            o2 = fmt_offset(offsets[2]),
            o3 = fmt_offset(offsets[3]),
        ))
        .unwrap()
    }

    #[test]
    fn port_offset_validates_per_host_computed_port_uniqueness_and_overflow() {
        // Distinct offsets per host (0 and 1 on host A; 0 and 1 on host B) -> valid!
        mixed_host_vm_config([Some(0), Some(1), Some(0), Some(1)])
            .validate()
            .unwrap();

        // Missing offsets on two founders sharing host A -> collision on computed ports -> rejected!
        let err = mixed_host_vm_config([None, None, Some(0), Some(1)])
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("port collision"), "{err}");

        // Cross-service port collision on host A: offset 97 shifts reth_p2p (30303 + 97 = 30400)
        // onto validator-0's consensus_p2p (30400) -> caught and rejected!
        let cross_err = mixed_host_vm_config([Some(0), Some(97), Some(0), Some(1)])
            .validate()
            .unwrap_err();
        assert!(
            cross_err.to_string().contains("30400"),
            "expected cross-service collision on 30400, got: {cross_err}"
        );

        // Overflow past u16::MAX (31303 + 40000 > 65535) -> rejected!
        let ovf_err = mixed_host_vm_config([Some(0), Some(40000), Some(0), Some(1)])
            .validate()
            .unwrap_err();
        assert!(format!("{ovf_err:#}").contains("overflows"), "{ovf_err:#}");

        // 1:1 P2P/Radicle publish port specs (guest == host == base + offset), Hyperlane fixed 9000.
        let p0 = FounderPublishedPorts::from_offset(0).unwrap();
        assert_eq!(
            p0.publish_port_specs(),
            vec![
                "30303/tcp",
                "30303/udp",
                "30400/tcp",
                "31303/udp",
                "8776/tcp",
                "9000/tcp",
            ]
        );
        let p1 = FounderPublishedPorts::from_offset(1).unwrap();
        assert_eq!(
            p1.publish_port_specs(),
            vec![
                "30304/tcp",
                "30304/udp",
                "30401/tcp",
                "31304/udp",
                "8777/tcp",
                "9000:9001/tcp",
            ]
        );

        // Missing static VM fields (prefix/gateway/dns) and rpc_bind_ip != bridge_ip are rejected.
        let mut missing_prefix = mixed_host_vm_config([Some(0), Some(1), Some(0), Some(1)]);
        missing_prefix.vm.as_mut().unwrap().prefix = None;
        assert!(missing_prefix
            .validate()
            .unwrap_err()
            .to_string()
            .contains("vm.prefix"));

        let mut mismatch_bind = mixed_host_vm_config([Some(0), Some(1), Some(0), Some(1)]);
        mismatch_bind.founders[0].rpc_bind_ip = Ipv4Addr::LOCALHOST;
        assert!(mismatch_bind
            .validate()
            .unwrap_err()
            .to_string()
            .contains("must equal vm.bridge_ip"));
    }

    #[test]
    fn single_host_vm_config_without_port_offsets_remains_valid() {
        let cfg: NetworkConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: single-host-devnet
network: devnet
chain_id: 424242
certified_rpc: "http://10.77.0.68:8545"
vm:
  vmm_host:
    host: 162.19.107.121
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
founders:
{}
hyperlane:
  gasless: true
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#,
            (0..4)
                .map(|i| format!(
                    "  - name: validator-{i}\n    advertise_ip: 10.77.0.{}\n    rpc_bind_ip: 10.77.0.{}\n    hyperlane_public_url: http://10.77.0.{}:9000\n    vm:\n      bridge_ip: 10.77.0.{}",
                    68 + i,
                    68 + i,
                    68 + i,
                    68 + i
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
        .unwrap();
        cfg.validate().unwrap();
    }
}
