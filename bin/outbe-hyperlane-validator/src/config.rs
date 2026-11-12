//! TOML configuration for the Hyperlane validator daemon.

use eyre::{Context, Result};
use serde::Deserialize;

use crate::errors::ConfigError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    pub chain: ChainConfig,
    pub account: AccountConfig,
    #[serde(default)]
    pub hyperlane: HyperlaneConfig,
    pub health: Option<HealthConfig>,
    /// Local MDBX store. The daemon and the `db` commands need it.
    pub storage: Option<StorageConfig>,
    /// Checkpoint HTTP server. The daemon needs it.
    pub server: Option<ServerConfig>,
    /// Origin chains whose MerkleTreeHook this validator signs.
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
    /// Hex-encoded private key `H`: the checkpoint signer, the announce
    /// sender and the `submitCheckpoint` sender. Its lowercase address is the
    /// bucket name.
    pub private_key: String,
    /// Validator `V` that this daemon acts for.
    pub validator_address: String,
}

/// Liveness and discovery settings.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlaneConfig {
    /// How often to read the controller and submit checkpoints (seconds).
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

/// The S3-compatible read endpoint that relayers use.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_server_bind")]
    pub bind_address: String,
    /// The public `http(s)://host[:port]` of this server. The announced
    /// storage location is built from it.
    pub public_url: String,
}

impl ServerConfig {
    /// `s3+http(s)://host[:port]`: the announced location without the bucket.
    pub fn location_base(&self) -> Result<String> {
        let invalid = || ConfigError::InvalidPublicUrl(self.public_url.clone());
        let url: reqwest::Url = self.public_url.parse().map_err(|_| invalid())?;
        let host = url.host_str().ok_or_else(invalid)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.path() != "/"
            || url.query().is_some()
            || !url.username().is_empty()
        {
            return Err(invalid().into());
        }
        let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
        Ok(format!("s3+{}://{host}{port}", url.scheme()))
    }
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
fn default_server_bind() -> String {
    "0.0.0.0:9000".to_string()
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
            return Err(ConfigError::InvalidValidatorAddress(
                self.account.validator_address.clone(),
            )
            .into());
        }
        if self.hyperlane.poll_interval_secs == 0 {
            return Err(ConfigError::ZeroPollInterval.into());
        }
        if self.storage.is_none() {
            return Err(ConfigError::NoDbPath.into());
        }
        self.server
            .as_ref()
            .ok_or(ConfigError::NoServer)?
            .location_base()?;
        let mut domains = std::collections::HashSet::new();
        for origin in &self.origin {
            if !domains.insert(origin.domain) {
                return Err(ConfigError::DuplicateOrigin(origin.domain).into());
            }
            if origin.rpc_urls.is_empty() {
                return Err(ConfigError::NoOriginRpc(origin.domain).into());
            }
            if origin.interval_secs == 0 {
                return Err(ConfigError::ZeroOriginInterval(origin.domain).into());
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

[storage]
db_path = "/tmp/db"

[server]
public_url = "http://203.0.113.7:9000"
"#;

    #[test]
    fn minimal_config_uses_defaults() {
        let cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.hyperlane.poll_interval_secs, 30);
        assert!(!cfg.hyperlane.gasless);
        assert!(cfg.health.is_none());
        assert_eq!(cfg.storage.unwrap().db_path.to_str(), Some("/tmp/db"));
        assert_eq!(cfg.server.unwrap().bind_address, "0.0.0.0:9000");
    }

    #[test]
    fn location_base_follows_public_url() {
        let base = |url: &str| {
            ServerConfig {
                bind_address: String::new(),
                public_url: url.to_string(),
            }
            .location_base()
        };
        assert_eq!(
            base("http://203.0.113.7:9000").unwrap(),
            "s3+http://203.0.113.7:9000"
        );
        assert_eq!(
            base("https://v.example.org/").unwrap(),
            "s3+https://v.example.org"
        );
        for bad in ["ftp://h:9000", "http://h:9000/path", "h:9000", "http://u@h"] {
            assert!(base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.hyperlane.poll_interval_secs = 0;
        assert!(cfg.validate().is_err());

        let mut cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.account.validator_address = "not-an-address".to_string();
        assert!(cfg.validate().is_err());

        let mut cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.server = None;
        assert!(cfg.validate().is_err());
        let mut cfg: ValidatorConfig = toml::from_str(MINIMAL).unwrap();
        cfg.storage = None;
        assert!(cfg.validate().is_err());

        assert!(toml::from_str::<ValidatorConfig>(&format!(
            "{MINIMAL}\n[hyperlane]\ns3_endpoint = \"x\"\n"
        ))
        .is_err());
    }

    /// The shipped example must parse and validate, so it cannot drift from
    /// the structs.
    #[test]
    fn example_config_parses_and_validates() {
        let cfg: ValidatorConfig =
            toml::from_str(include_str!("../hyperlane-validator.example.toml")).unwrap();
        cfg.validate().unwrap();
        let domains: Vec<u32> = cfg.origin.iter().map(|o| o.domain).collect();
        assert_eq!(domains, [54322345, 97, 11155111]);
        assert_eq!(cfg.origin[0].reorg_period, ReorgPeriod::Blocks(0));
        assert_eq!(cfg.origin[0].ws_url, None);
        assert_eq!(cfg.origin[1].rpc_urls.len(), 3);
        assert!(cfg.origin.iter().all(|o| o.interval_secs == 15));
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
}
