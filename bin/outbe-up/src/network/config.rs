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
use serde::{Deserialize, Serialize};

use crate::support::progress::Progress;
use crate::validator::config::{HyperlaneConfig, HyperlaneOrigin};

pub const CONFIG_VERSION: u32 = 1;
pub const MIN_FOUNDERS: usize = 4;
pub const EXAMPLE: &str = include_str!("../../templates/network/network.example.yaml");

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
    pub origins: Vec<HyperlaneOrigin>,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    pub host: String,
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
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
        }

        let mut names = BTreeSet::new();
        let mut ssh_targets = BTreeSet::new();
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
            }
            ensure!(
                advertise_ips.insert(founder.advertise_ip),
                "duplicate advertise_ip: {}",
                founder.advertise_ip
            );
            ensure!(
                founder.rpc_bind_ip.is_loopback() || founder.rpc_bind_ip.is_private(),
                "rpc_bind_ip for {} must be loopback or private",
                founder.name
            );
            self.hyperlane
                .for_public_url(founder.hyperlane_public_url.clone())
                .validate()
                .wrap_err_with(|| format!("invalid Hyperlane config for {}", founder.name))?;
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
}

fn validate_ssh(ssh: &Ssh, label: &str) -> Result<()> {
    ensure!(safe_host(&ssh.host), "invalid SSH host for {label}");
    ensure!(safe_user(&ssh.user), "invalid SSH user for {label}");
    ensure!(ssh.port > 0, "SSH port must be non-zero for {label}");
    Ok(())
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

fn safe_host(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'_'))
}

fn safe_user(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

const fn loopback() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

fn default_ssh_user() -> String {
    "ubuntu".to_owned()
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
}
