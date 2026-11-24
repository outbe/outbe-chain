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
    pub machines: BTreeMap<String, Machine>,
    pub founders: Vec<Founder>,
    pub hyperlane: HyperlaneNetworkConfig,
    #[serde(default)]
    pub topology: Topology,
    pub genesis: Genesis,
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
    pub machine: String,
    pub private_ip: Ipv4Addr,
    #[serde(default)]
    pub publish_rpc: bool,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    pub fn has_shared_validator_machine(&self) -> bool {
        let mut machines = BTreeSet::new();
        self.founders
            .iter()
            .any(|founder| !machines.insert(founder.machine.as_str()))
    }

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

        let mut names = BTreeSet::new();
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

    pub fn resolve_topology(&self) -> Result<Vec<ResolvedFounder>> {
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

    #[test]
    fn detects_shared_validator_machines() {
        let mut config: NetworkConfig = serde_yaml_ng::from_str(EXAMPLE).unwrap();
        assert!(config.has_shared_validator_machine());

        let machine = config.machines.values().next().unwrap().clone();
        for (index, founder) in config.founders.iter_mut().enumerate() {
            founder.machine = format!("host-{index}");
            config
                .machines
                .insert(founder.machine.clone(), machine.clone());
        }
        assert!(!config.has_shared_validator_machine());
    }
}
