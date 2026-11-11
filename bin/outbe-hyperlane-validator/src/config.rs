//! TOML configuration for the Hyperlane validator daemon.

use eyre::{Context, Result};
use serde::Deserialize;

use crate::errors::ValidatorError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    pub chain: ChainConfig,
    pub account: AccountConfig,
    #[serde(default)]
    pub hyperlane: HyperlaneConfig,
    pub health: Option<HealthConfig>,
    /// Local MDBX store. Only the `db` commands use it for now.
    pub storage: Option<StorageConfig>,
    /// Origin chains whose MerkleTreeHook this validator signs. The daemon
    /// does not use them yet.
    #[serde(default)]
    pub origin: Vec<OriginConfig>,
}

/// One origin chain. Contract addresses come from the HyperlaneController
/// and the chain itself, not from this file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginConfig {
    pub domain: u32,
    /// JSON-RPC endpoints (HTTP). Each one has one vote on the hook root.
    pub rpc_urls: Vec<String>,
    #[serde(default)]
    pub rpc_consensus: RpcConsensus,
    pub reorg_period: ReorgPeriod,
    /// Poll interval without WebSocket, and the fallback poll with it.
    #[serde(default = "default_origin_interval")]
    pub interval_secs: u64,
    /// Optional WebSocket endpoint. Its events only wake the poll earlier.
    pub ws_url: Option<String>,
    /// First block to read. Default: `deployedBlock()` of the hook.
    pub index_from: Option<u64>,
}

/// Number of `rpc_urls` that must return the same root before the validator
/// signs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcConsensus {
    /// N - floor(N/3): at least 2/3.
    #[default]
    Majority,
    /// ceil(N/2): at least 1/2.
    Quorum,
    /// One endpoint is sufficient. No vote.
    Single,
}

impl RpcConsensus {
    /// Agreeing endpoints that a pool of `endpoints` needs.
    pub fn required(self, endpoints: usize) -> usize {
        match self {
            Self::Majority => endpoints - endpoints / 3,
            Self::Quorum => endpoints.div_ceil(2),
            Self::Single => 1,
        }
    }
}

/// How far behind the chain head a block is final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ReorgPeriod {
    /// `latest - n`.
    Blocks(u64),
    /// The `finalized` block tag of the chain.
    Tag(FinalityTag),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FinalityTag {
    Finalized,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainConfig {
    /// Outbe JSON-RPC endpoint (HTTP).
    pub rpc_endpoint: String,
    pub chain_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    /// Hex-encoded private key of the validator's Hyperlane delegate.
    /// The validator key is accepted when no Hyperlane delegate is assigned.
    pub private_key: String,
    /// Validator that this daemon acts for. It is also the checkpoint bucket name.
    pub validator_address: String,
}

/// Checkpoint submission settings. The daemon reads this validator's own
/// public-read bucket (the location it announced on-chain, `<bucket>/<domain>/`)
/// for every domain that the HyperlaneController knows. It submits the latest
/// signed checkpoint of each domain.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlaneConfig {
    /// How often to poll the bucket (seconds).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Submit through the ZeroFee txpool policy.
    #[serde(default)]
    pub gasless: bool,
}

impl Default for HyperlaneConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
            gasless: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Directory of the validator's own MDBX store. Never the node database.
    pub db_path: std::path::PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_health_bind")]
    pub bind_address: String,
}

fn default_true() -> bool {
    true
}
fn default_health_bind() -> String {
    "0.0.0.0:9003".to_string()
}
fn default_poll_interval() -> u64 {
    30
}
fn default_origin_interval() -> u64 {
    15
}

impl ValidatorConfig {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {path}"))?;
        toml::from_str(&content).with_context(|| "failed to parse hyperlane validator config")
    }

    pub fn validate(&self) -> Result<()> {
        let addr = self.account.validator_address.trim_start_matches("0x");
        if addr.len() != 40 || addr.chars().any(|c| !c.is_ascii_hexdigit()) {
            return Err(ValidatorError::InvalidValidatorAddress(
                self.account.validator_address.clone(),
            )
            .into());
        }
        if self.hyperlane.poll_interval_secs == 0 {
            return Err(ValidatorError::ZeroPollInterval.into());
        }
        let mut domains = std::collections::HashSet::new();
        for origin in &self.origin {
            if !domains.insert(origin.domain) {
                return Err(ValidatorError::DuplicateOrigin(origin.domain).into());
            }
            if origin.rpc_urls.is_empty() {
                return Err(ValidatorError::NoOriginRpc(origin.domain).into());
            }
            if origin.interval_secs == 0 {
                return Err(ValidatorError::ZeroOriginInterval(origin.domain).into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[chain]
rpc_endpoint = "http://127.0.0.1:8545"
chain_id = 54322345

[account]
private_key = "0xdead"
validator_address = "0x1111111111111111111111111111111111111111"
"#;

    #[test]
    fn minimal_config_uses_defaults() {
        let cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.hyperlane.poll_interval_secs, 30);
        assert!(!cfg.hyperlane.gasless);
        assert!(cfg.health.is_none());
        assert!(cfg.storage.is_none());

        let cfg: ValidatorConfig =
            toml::from_str(&format!("{MINIMAL}\n[storage]\ndb_path = \"/tmp/db\"\n")).unwrap();
        assert_eq!(cfg.storage.unwrap().db_path.to_str(), Some("/tmp/db"));
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.hyperlane.poll_interval_secs = 0;
        assert!(cfg.validate().is_err());

        let mut cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.account.validator_address = "not-an-address".to_string();
        assert!(cfg.validate().is_err());

        assert!(toml::from_str::<ValidatorConfig>(&format!(
            "{MINIMAL}\n[hyperlane]\ns3_endpoint = \"x\"\n"
        ))
        .is_err());
    }

    const ORIGINS: &str = r#"
[[origin]]
domain = 54322345
rpc_urls = ["https://a", "https://b", "https://c"]
reorg_period = 0

[[origin]]
domain = 11155111
rpc_urls = ["https://d"]
rpc_consensus = "single"
reorg_period = "finalized"
interval_secs = 5
ws_url = "wss://d"
index_from = 7
"#;

    #[test]
    fn origins_parse_with_defaults() {
        let cfg: ValidatorConfig = toml::from_str(&format!("{MINIMAL}{ORIGINS}")).unwrap();
        cfg.validate().unwrap();
        assert!(toml::from_str::<ValidatorConfig>(MINIMAL)
            .unwrap()
            .origin
            .is_empty());
        let [outbe, sepolia] = cfg.origin.as_slice() else {
            panic!("two origins")
        };
        assert_eq!(outbe.rpc_consensus, RpcConsensus::Majority);
        assert_eq!(outbe.reorg_period, ReorgPeriod::Blocks(0));
        assert_eq!(outbe.interval_secs, 15);
        assert_eq!(outbe.ws_url, None);
        assert_eq!(sepolia.rpc_consensus, RpcConsensus::Single);
        assert_eq!(
            sepolia.reorg_period,
            ReorgPeriod::Tag(FinalityTag::Finalized)
        );
        assert_eq!(sepolia.index_from, Some(7));

        let bad_tag = ORIGINS.replace("\"finalized\"", "\"safe\"");
        assert!(toml::from_str::<ValidatorConfig>(&format!("{MINIMAL}{bad_tag}")).is_err());
        let duplicate = format!("{MINIMAL}{ORIGINS}{ORIGINS}");
        let cfg: ValidatorConfig = toml::from_str(&duplicate).unwrap();
        assert!(cfg.validate().is_err());
        let no_rpc = ORIGINS.replace("[\"https://d\"]", "[]");
        let cfg: ValidatorConfig = toml::from_str(&format!("{MINIMAL}{no_rpc}")).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn consensus_thresholds_match_upstream() {
        for (n, majority, quorum) in [(1, 1, 1), (2, 2, 1), (3, 2, 2), (4, 3, 2), (6, 4, 3)] {
            assert_eq!(RpcConsensus::Majority.required(n), majority, "majority {n}");
            assert_eq!(RpcConsensus::Quorum.required(n), quorum, "quorum {n}");
            assert_eq!(RpcConsensus::Single.required(n), 1);
        }
    }
}
