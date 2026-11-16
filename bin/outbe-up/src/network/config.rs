use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::Path,
    str::FromStr,
};

use alloy_primitives::{Address, U256};
use eyre::{ensure, Context, Result};
pub use outbe_software_bundle::SoftwareProfile;
use serde::{Deserialize, Serialize};

use crate::validator::config::EnclaveRuntime;

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
    pub software_profile: SoftwareProfile,
    pub founders: Vec<Founder>,
    #[serde(default)]
    pub topology: Topology,
    pub genesis: Genesis,
    pub tee: Tee,
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
    pub ssh: Ssh,
    pub advertise_ip: Ipv4Addr,
    #[serde(default = "loopback")]
    pub rpc_bind_ip: Ipv4Addr,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tee {
    pub policy: TeePolicy,
    pub runtime: EnclaveRuntime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeePolicy {
    GramineDirectDev,
    DcapRequired,
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
            self.founders.len() >= MIN_FOUNDERS,
            "at least {MIN_FOUNDERS} founding validators are required"
        );

        let mut names = BTreeSet::new();
        let mut ssh_targets = BTreeSet::new();
        let mut advertise_ips = BTreeSet::new();
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
                safe_host(&founder.ssh.host),
                "invalid SSH host for {}",
                founder.name
            );
            ensure!(
                safe_user(&founder.ssh.user),
                "invalid SSH user for {}",
                founder.name
            );
            ensure!(
                founder.ssh.port > 0,
                "SSH port must be non-zero for {}",
                founder.name
            );
            ensure!(
                ssh_targets.insert((&founder.ssh.host, founder.ssh.port)),
                "duplicate SSH target: {}:{}",
                founder.ssh.host,
                founder.ssh.port
            );
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
        let reserved = reserved_genesis_keys();
        if let Some(key) = self
            .genesis
            .overrides
            .keys()
            .find(|key| reserved.contains(key.as_str()))
        {
            eyre::bail!("genesis.overrides may not replace managed key `{key}`");
        }

        let runtime_is_compatible = matches!(
            (self.software_profile, self.tee.policy, self.tee.runtime),
            (
                SoftwareProfile::Development,
                TeePolicy::GramineDirectDev,
                EnclaveRuntime::Mock
            )
        ) || matches!(
            (self.software_profile, self.tee.policy, self.tee.runtime),
            (SoftwareProfile::Production, _, runtime)
                if !matches!(runtime, EnclaveRuntime::Mock)
        );
        ensure!(
            runtime_is_compatible,
            "software_profile, tee.policy, and tee.runtime select an incompatible runtime"
        );
        ensure!(
            self.network != Network::Mainnet || self.tee.policy == TeePolicy::DcapRequired,
            "mainnet requires tee.policy=dcap-required"
        );
        Ok(())
    }
}

pub fn init(path: &Path) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create network config: {}", path.display()))?;
    file.write_all(EXAMPLE.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    NetworkConfig::read(path)?;
    println!("network config: {}", path.display());
    Ok(())
}

fn reserved_genesis_keys() -> BTreeSet<&'static str> {
    [
        "network",
        "chain_id",
        "validators",
        "keys_dir",
        "tee",
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
        "enclave_sgx",
        "enclave_dir",
        "enclave_runner",
        "signed_enclave_dir",
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
software_profile: development
founders:
{}
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts:
    "0x1111111111111111111111111111111111111111": "1000000000000000000"
tee:
  policy: gramine-direct-dev
  runtime: mock
"#,
            (0..founder_count)
                .map(|index| format!(
                    "  - name: validator-{index}\n    ssh:\n      host: 192.0.2.{}\n    advertise_ip: 10.0.0.{}",
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
    fn separates_software_and_tee_modes() {
        let mut config = config_with(4);
        config.software_profile = SoftwareProfile::Production;
        config.tee.runtime = EnclaveRuntime::SystemGramine;
        config.validate().unwrap();
        config.tee.policy = TeePolicy::DcapRequired;
        config.validate().unwrap();

        config.software_profile = SoftwareProfile::Development;
        assert!(config.validate().is_err());
    }

    #[test]
    fn example_is_valid() {
        let config: NetworkConfig = serde_yaml_ng::from_str(EXAMPLE).unwrap();
        config.validate().unwrap();
    }
}
