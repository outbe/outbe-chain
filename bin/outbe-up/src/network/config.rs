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
use outbe_operator_remote::validate_ssh;
pub use outbe_operator_remote::Ssh;
use serde::{Deserialize, Serialize};

use outbe_operator_node_role::{
    config::{HyperlaneConfig, HyperlaneOrigin, ReorgPeriod, RpcConsensus},
    progress::Progress,
};

pub const CONFIG_VERSION: u32 = 2;
pub const MIN_FOUNDERS: usize = 4;
pub const EXAMPLE: &str = include_str!("../../templates/network/network.example.yaml");

const RETH_P2P_BASE_PORT: u16 = 30_303;
const RETH_DISCOVERY_BASE_PORT: u16 = 31_303;
const CONSENSUS_BASE_PORT: u16 = 30_400;
const RADICLE_BASE_PORT: u16 = 8_776;
const HYPERLANE_BASE_PORT: u16 = 9_000;
const RPC_BASE_PORT: u16 = 8_545;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub version: u32,
    pub name: String,
    pub network: Network,
    pub chain_id: u64,
    pub certified_rpc: String,
    #[serde(default)]
    pub machines: BTreeMap<String, Machine>,
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
    /// Host dnsmasq DHCP hosts file used for static `mac,ip` reservations.
    #[serde(default = "default_vm_dhcp_hosts_file")]
    pub dhcp_hosts_file: PathBuf,
    /// Host dnsmasq DHCP leases file checked for active IP conflicts.
    #[serde(default = "default_vm_dhcp_leases_file")]
    pub dhcp_leases_file: PathBuf,
    /// Host dnsmasq PID file signaled with SIGHUP after reservation updates.
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFounderVm {
    pub vmm_host: Ssh,
    pub vmm_home: String,
    pub bridge: String,
    pub dhcp_hosts_file: PathBuf,
    pub dhcp_leases_file: PathBuf,
    pub dhcp_pid_file: PathBuf,
    pub bridge_ip: Ipv4Addr,
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
    #[serde(default = "loopback")]
    pub private_ip: Ipv4Addr,
    #[serde(default)]
    pub publish_rpc: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise_ip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_bind_ip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlane_public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<FounderVmConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub public_ip: Ipv4Addr,
    #[serde(default = "default_ssh_user")]
    pub ssh_user: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    #[serde(default = "default_vm_ssh_user")]
    pub vm_ssh_user: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedFounder {
    pub name: String,
    pub machine: String,
    pub public_ip: Ipv4Addr,
    pub private_ip: Ipv4Addr,
    pub offset: u16,
    pub jump_user: String,
    pub jump_port: u16,
    pub vm_user: String,
    pub ports: ResolvedPorts,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedPorts {
    pub reth_p2p: u16,
    pub reth_discovery: u16,
    pub consensus: u16,
    pub radicle: u16,
    pub hyperlane_public: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_public: Option<u16>,
}

impl ResolvedFounder {
    pub fn consensus_endpoint(&self) -> String {
        format!("{}:{}", self.public_ip, self.ports.consensus)
    }

    pub fn hyperlane_public_url(&self) -> String {
        format!("http://{}:{}", self.public_ip, self.ports.hyperlane_public)
    }
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
    /// When set on a VM network, `origins` must be empty and `outbe-up` generates
    /// exactly two origins: the local Outbe node and this local EVM origin.
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
        if self.vm.is_some() {
            ensure!(
                self.version == 1 || self.version == CONFIG_VERSION,
                "unsupported network config version {}",
                self.version
            );
        } else {
            ensure!(
                self.version == CONFIG_VERSION,
                "unsupported network config version {}",
                self.version
            );
        }
        ensure!(
            safe_name(&self.name),
            "name must use letters, digits, `-`, or `_`"
        );
        ensure!(
            self.chain_id == self.network.chain_id(),
            "{} requires chain_id {}",
            self.network.as_str(),
            self.network.chain_id()
        );
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

        let mut names = BTreeSet::new();
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
            let mut advertise_ips = BTreeSet::new();
            let mut bridge_ips = BTreeSet::new();
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
                    bridge_ips.insert(resolved.bridge_ip),
                    "duplicate vm.bridge_ip: {}",
                    resolved.bridge_ip
                );
                if resolved.flavor == VmFlavor::Prod {
                    ensure!(
                        resolved.authorized_keys.is_none(),
                        "prod VM flavor does not allow authorized_keys for {}",
                        founder.name
                    );
                }
                let advertise_ip = founder.advertise_ip.ok_or_else(|| {
                    eyre::eyre!("missing advertise_ip for VM founder {}", founder.name)
                })?;
                ensure!(
                    advertise_ips.insert(advertise_ip),
                    "duplicate advertise_ip: {advertise_ip}"
                );
                let rpc_bind_ip = founder.rpc_bind_ip.unwrap_or(Ipv4Addr::LOCALHOST);
                ensure!(
                    rpc_bind_ip.is_loopback() || rpc_bind_ip.is_private(),
                    "rpc_bind_ip for {} must be loopback or private",
                    founder.name
                );
                self.resolve_hyperlane_config(founder)
                    .wrap_err_with(|| format!("invalid Hyperlane config for {}", founder.name))?;
            }
        } else {
            ensure!(
                self.hyperlane.local_evm.is_none(),
                "hyperlane.local_evm is only supported for VM deployments"
            );
            ensure!(
                !self.machines.is_empty(),
                "at least one machine is required"
            );
            let mut public_ips = BTreeSet::new();
            for (name, machine) in &self.machines {
                ensure!(safe_name(name), "invalid machine name: {name}");
                ensure!(
                    !machine.public_ip.is_unspecified()
                        && !machine.public_ip.is_multicast()
                        && machine.public_ip != Ipv4Addr::BROADCAST,
                    "invalid public_ip for machine {name}: {}",
                    machine.public_ip
                );
                ensure!(
                    public_ips.insert(machine.public_ip),
                    "duplicate machine public_ip: {}",
                    machine.public_ip
                );
                ensure!(
                    safe_user(&machine.ssh_user),
                    "invalid SSH user for machine {name}"
                );
                ensure!(
                    safe_user(&machine.vm_ssh_user),
                    "invalid VM SSH user for machine {name}"
                );
                ensure!(
                    machine.ssh_port > 0,
                    "SSH port must be non-zero for machine {name}"
                );
            }

            let mut private_ips = BTreeSet::new();
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
                ensure!(
                    founder.vm.is_none(),
                    "founder {} sets vm without a top-level vm section",
                    founder.name
                );
                ensure!(
                    self.machines.contains_key(&founder.machine),
                    "unknown machine `{}` for {}",
                    founder.machine,
                    founder.name
                );
                ensure!(
                    founder.private_ip.is_private(),
                    "private_ip for {} must be a private IPv4 address",
                    founder.name
                );
                ensure!(
                    private_ips.insert((&founder.machine, founder.private_ip)),
                    "duplicate private_ip {} on machine {}",
                    founder.private_ip,
                    founder.machine
                );
            }

            self.resolve_topology()?;
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
            dhcp_hosts_file: defaults.dhcp_hosts_file.clone(),
            dhcp_leases_file: defaults.dhcp_leases_file.clone(),
            dhcp_pid_file: defaults.dhcp_pid_file.clone(),
            bridge_ip: override_cfg.bridge_ip,
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
        let public_url = founder
            .hyperlane_public_url
            .clone()
            .unwrap_or_else(|| "http://192.0.2.10:9000".into());
        let local_rpc_ip = founder.rpc_bind_ip.unwrap_or(founder.private_ip);
        self.resolve_hyperlane_for_endpoint(public_url, local_rpc_ip)
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

    pub fn resolve_topology(&self) -> Result<Vec<ResolvedFounder>> {
        if self.vm.is_some() {
            let mut resolved = Vec::with_capacity(self.founders.len());
            for founder in &self.founders {
                let advertise_ip = founder.advertise_ip.ok_or_else(|| {
                    eyre::eyre!("missing advertise_ip for VM founder {}", founder.name)
                })?;
                let rpc_bind_ip = founder.rpc_bind_ip.unwrap_or(Ipv4Addr::LOCALHOST);
                let vm = self.resolve_founder_vm(founder)?;
                resolved.push(ResolvedFounder {
                    name: founder.name.clone(),
                    machine: vm.vmm_host.host.clone(),
                    public_ip: advertise_ip,
                    private_ip: rpc_bind_ip,
                    offset: 0,
                    jump_user: vm.vmm_host.user,
                    jump_port: vm.vmm_host.port,
                    vm_user: default_vm_ssh_user(),
                    ports: ResolvedPorts {
                        reth_p2p: RETH_P2P_BASE_PORT,
                        reth_discovery: RETH_DISCOVERY_BASE_PORT,
                        consensus: CONSENSUS_BASE_PORT,
                        radicle: RADICLE_BASE_PORT,
                        hyperlane_public: HYPERLANE_BASE_PORT,
                        rpc_public: None,
                    },
                });
            }
            return Ok(resolved);
        }

        let mut offsets = BTreeMap::<&str, u16>::new();
        let mut endpoints = BTreeMap::<(Ipv4Addr, &'static str, u16), String>::new();
        let mut resolved = Vec::with_capacity(self.founders.len());
        for founder in &self.founders {
            let machine = self
                .machines
                .get(&founder.machine)
                .ok_or_else(|| eyre::eyre!("unknown machine `{}`", founder.machine))?;
            let offset = *offsets.entry(&founder.machine).or_default();
            offsets.insert(
                &founder.machine,
                offset
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("too many validators on {}", founder.machine))?,
            );
            let ports = ResolvedPorts {
                reth_p2p: port_with_offset(RETH_P2P_BASE_PORT, offset, &founder.name)?,
                reth_discovery: port_with_offset(RETH_DISCOVERY_BASE_PORT, offset, &founder.name)?,
                consensus: port_with_offset(CONSENSUS_BASE_PORT, offset, &founder.name)?,
                radicle: port_with_offset(RADICLE_BASE_PORT, offset, &founder.name)?,
                hyperlane_public: port_with_offset(HYPERLANE_BASE_PORT, offset, &founder.name)?,
                rpc_public: founder
                    .publish_rpc
                    .then(|| port_with_offset(RPC_BASE_PORT, offset, &founder.name))
                    .transpose()?,
            };
            for (protocol, port) in [
                ("tcp", ports.reth_p2p),
                ("udp", ports.reth_p2p),
                ("udp", ports.reth_discovery),
                ("tcp", ports.consensus),
                ("tcp", ports.radicle),
                ("tcp", ports.hyperlane_public),
            ]
            .into_iter()
            .chain(ports.rpc_public.map(|port| ("tcp", port)))
            {
                let key = (machine.public_ip, protocol, port);
                ensure!(
                    endpoints.insert(key, founder.name.clone()).is_none(),
                    "public endpoint collision on {} {protocol}/{port}",
                    machine.public_ip
                );
            }
            let value = ResolvedFounder {
                name: founder.name.clone(),
                machine: founder.machine.clone(),
                public_ip: machine.public_ip,
                private_ip: founder.private_ip,
                offset,
                jump_user: machine.ssh_user.clone(),
                jump_port: machine.ssh_port,
                vm_user: machine.vm_ssh_user.clone(),
                ports,
            };
            self.hyperlane
                .for_public_url(value.hyperlane_public_url())
                .validate()
                .wrap_err_with(|| format!("invalid Hyperlane config for {}", founder.name))?;
            resolved.push(value);
        }
        Ok(resolved)
    }
}

fn port_with_offset(base: u16, offset: u16, founder: &str) -> Result<u16> {
    base.checked_add(offset)
        .ok_or_else(|| eyre::eyre!("port range exhausted for {founder}"))
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
        "validator_runtime",
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
        "reth_p2p_port",
        "reth_discv5_port",
        "radicle_port",
        "consensus_p2p_port",
        "hyperlane_public_urls",
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

fn safe_user(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn default_ssh_user() -> String {
    "ubuntu".to_owned()
}

fn default_vm_ssh_user() -> String {
    "root".to_owned()
}

const fn default_ssh_port() -> u16 {
    22
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
    PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.hosts")
}

pub(crate) fn default_vm_dhcp_leases_file() -> PathBuf {
    PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.leases")
}

fn default_vm_dhcp_pid_file() -> PathBuf {
    PathBuf::from("/run/outbe-mvp-dnsmasq.pid")
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
version: 2
name: local-testnet
network: testnet
chain_id: 54322345
certified_rpc: "http://10.0.0.10:8545"
machines:
  host:
    public_ip: 192.0.2.10
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
                    "  - name: validator-{index}\n    machine: host\n    private_ip: 10.0.0.{}",
                    index + 10,
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

    fn vm_config(network: &str, chain_id: u64) -> NetworkConfig {
        serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: vm-devnet
network: {network}
chain_id: {chain_id}
certified_rpc: "http://10.0.0.10:8545"
vm:
  vmm_host:
    host: 192.0.2.50
  authorized_keys: ./dev.pub
founders:
  - name: validator-0
    advertise_ip: 10.0.0.10
    rpc_bind_ip: 10.0.0.10
    hyperlane_public_url: http://192.0.2.50:9000
    vm:
      bridge_ip: 10.77.0.10
  - name: validator-1
    advertise_ip: 10.0.0.11
    rpc_bind_ip: 10.0.0.11
    hyperlane_public_url: http://192.0.2.50:9001
    vm:
      bridge_ip: 10.77.0.11
      vcpu: 4
      memory_gib: 8
  - name: validator-2
    advertise_ip: 10.0.0.12
    rpc_bind_ip: 10.0.0.12
    hyperlane_public_url: http://192.0.2.51:9000
    vm:
      bridge_ip: 10.77.0.12
      vmm_host:
        host: 192.0.2.51
        user: root
        port: 2222
  - name: validator-3
    advertise_ip: 10.0.0.13
    rpc_bind_ip: 10.0.0.13
    hyperlane_public_url: http://192.0.2.51:9001
    vm:
      bridge_ip: 10.77.0.13
hyperlane:
  origins:
    - domain: {chain_id}
      rpc_urls: ["https://rpc.example.invalid"]
      reorg_period: 0
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#
        ))
        .unwrap()
    }

    #[test]
    fn vm_config_resolves_defaults_and_overrides() {
        let config = vm_config("devnet", Network::Devnet.chain_id());
        config.validate().unwrap();

        let f0 = config.resolve_founder_vm(&config.founders[0]).unwrap();
        assert_eq!(f0.vmm_host.host, "192.0.2.50");
        assert_eq!(f0.vcpu, 3);
        assert_eq!(f0.memory_gib, 5);
        assert_eq!(f0.disk_gib, 40);
        assert_eq!(f0.flavor, VmFlavor::Dev);
        assert_eq!(f0.bridge_ip, Ipv4Addr::new(10, 77, 0, 10));
        assert_eq!(f0.authorized_keys, Some(PathBuf::from("./dev.pub")));

        let f1 = config.resolve_founder_vm(&config.founders[1]).unwrap();
        assert_eq!(f1.vcpu, 4);
        assert_eq!(f1.memory_gib, 8);
        assert_eq!(f1.disk_gib, 40);

        let f2 = config.resolve_founder_vm(&config.founders[2]).unwrap();
        assert_eq!(f2.vmm_host.host, "192.0.2.51");
        assert_eq!(f2.vmm_host.user, "root");
        assert_eq!(f2.vmm_host.port, 2222);
    }

    #[test]
    fn vm_config_rejects_mainnet_and_invalid_combinations() {
        let mainnet = vm_config("mainnet", Network::Mainnet.chain_id());
        let err = mainnet.validate().unwrap_err();
        assert!(err.to_string().contains("mainnet"), "{err}");

        let mut prod_with_keys = vm_config("devnet", Network::Devnet.chain_id());
        prod_with_keys.vm.as_mut().unwrap().flavor = VmFlavor::Prod;
        let err = prod_with_keys.validate().unwrap_err();
        assert!(err.to_string().contains("authorized_keys"), "{err}");

        let mut duplicate_bridge = vm_config("devnet", Network::Devnet.chain_id());
        duplicate_bridge.founders[1].vm.as_mut().unwrap().bridge_ip = Ipv4Addr::new(10, 77, 0, 10);
        let err = duplicate_bridge.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate vm.bridge_ip"), "{err}");
    }

    #[test]
    fn resolves_offsets_per_machine() {
        let config = config_with(4);
        let founders = config.resolve_topology().unwrap();
        assert_eq!(founders[0].ports.reth_p2p, 30303);
        assert_eq!(founders[1].ports.reth_p2p, 30304);
        assert_eq!(founders[3].ports.hyperlane_public, 9003);
        assert_eq!(founders[0].ports.rpc_public, None);
        assert_eq!(
            founders[0].private_ip,
            "10.0.0.10".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(
            founders[0].public_ip,
            "192.0.2.10".parse::<Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn offsets_restart_on_each_machine() {
        let config: NetworkConfig = serde_yaml_ng::from_str(EXAMPLE).unwrap();
        let founders = config.resolve_topology().unwrap();
        assert_eq!(founders[0].offset, 0);
        assert_eq!(founders[1].offset, 1);
        assert_eq!(founders[2].offset, 0);
        assert_eq!(founders[3].offset, 1);
        assert_eq!(founders[0].ports.rpc_public, Some(8545));
        assert_eq!(founders[1].ports.rpc_public, None);
    }
}
