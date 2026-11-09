use std::{net::Ipv4Addr, path::Path};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub version: u32,
    pub network: NetworkConfig,
    pub tee: TeeConfig,
    pub feeder: FeederConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub chain_id: u64,
    /// Genesis block hash, lowercase without a 0x prefix.
    pub genesis_hash: String,
    pub genesis_sha256: String,
    pub certified_rpc: String,
    pub external_ip: Ipv4Addr,
    pub bootnodes: Vec<String>,
    pub consensus_peers: Vec<String>,
    #[serde(default)]
    pub use_local_defaults: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeeConfig {
    pub runtime: EnclaveRuntime,
    pub lease_duration_seconds: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnclaveRuntime {
    BundledSgx,
    SystemGramine,
}

impl EnclaveRuntime {
    pub const fn as_env(self) -> &'static str {
        match self {
            Self::BundledSgx => "bundled-sgx",
            Self::SystemGramine => "system-gramine",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeederConfig {
    /// Public TOML appended to the locally generated account configuration.
    pub public_toml: String,
}

impl OperatorConfig {
    pub fn read(path: &Path) -> Result<(Self, String)> {
        let raw = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read operator config: {}", path.display()))?;
        let config: Self = serde_yaml_ng::from_str(&raw)
            .wrap_err_with(|| format!("invalid operator config: {}", path.display()))?;
        config.validate()?;
        Ok((config, sha256(raw.as_bytes())))
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == CONFIG_VERSION,
            "unsupported operator config version {}",
            self.version
        );
        ensure!(
            self.network.chain_id > 0,
            "network.chain_id must be positive"
        );
        ensure!(
            is_sha256(&self.network.genesis_hash),
            "network.genesis_hash must be 64 lowercase hex characters"
        );
        ensure!(
            is_sha256(&self.network.genesis_sha256),
            "network.genesis_sha256 must be 64 lowercase hex characters"
        );
        ensure!(
            self.network.certified_rpc.starts_with("http://")
                || self.network.certified_rpc.starts_with("https://"),
            "network.certified_rpc must be an HTTP(S) URL"
        );
        ensure!(
            !self.network.bootnodes.is_empty(),
            "network.bootnodes is empty"
        );
        if self
            .network
            .bootnodes
            .iter()
            .any(|node| !node.starts_with("enode://") || node.contains(char::is_whitespace))
        {
            bail!("network.bootnodes contains an invalid enode URL");
        }
        ensure!(
            !self.network.consensus_peers.is_empty(),
            "network.consensus_peers is empty"
        );
        if self
            .network
            .consensus_peers
            .iter()
            .any(|peer| !valid_consensus_peer(peer))
        {
            bail!("network.consensus_peers contains an invalid peer");
        }
        ensure!(
            self.tee.lease_duration_seconds > 0,
            "tee.lease_duration_seconds must be positive"
        );
        ensure!(
            !self.feeder.public_toml.trim().is_empty(),
            "feeder.public_toml is empty"
        );
        self.feeder
            .public_toml
            .parse::<toml::Table>()
            .wrap_err("feeder.public_toml is not valid TOML")?;
        Ok(())
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_consensus_peer(value: &str) -> bool {
    let Some((key, address)) = value.split_once('@') else {
        return false;
    };
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    key.len() == 96
        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        && host.parse::<Ipv4Addr>().is_ok()
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_minimal_config() {
        let config: OperatorConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
network:
  chain_id: 424242
  genesis_hash: {}
  genesis_sha256: {}
  certified_rpc: https://rpc.example
  external_ip: 192.0.2.10
  bootnodes: ["enode://abc@192.0.2.1:30303"]
  consensus_peers: ["{}@192.0.2.1:30400"]
  use_local_defaults: true
tee:
  runtime: bundled-sgx
  lease_duration_seconds: 86400
feeder:
  public_toml: |
    [oracle]
    sources = "chain"
"#,
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(96),
        ))
        .unwrap();
        config.validate().unwrap();
    }
}
