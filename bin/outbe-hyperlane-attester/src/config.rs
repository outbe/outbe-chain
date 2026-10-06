//! TOML configuration for the Hyperlane attester daemon.

use eyre::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttesterConfig {
    pub chain: ChainConfig,
    pub account: AccountConfig,
    #[serde(default)]
    pub hyperlane: HyperlaneConfig,
    pub health: Option<HealthConfig>,
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
    /// Hex-encoded private key of the attester account: the validator's oracle
    /// delegate (same key as outbe-feeder) or the validator itself.
    pub private_key: String,
    /// Validator that this attester acts for. It is also the checkpoint bucket name.
    pub validator_address: String,
}

/// Checkpoint submission settings. The attester reads this validator's own
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

impl AttesterConfig {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {path}"))?;
        toml::from_str(&content).with_context(|| "failed to parse hyperlane attester config")
    }

    pub fn validate(&self) -> Result<()> {
        let addr = self.account.validator_address.trim_start_matches("0x");
        if addr.len() != 40 || addr.chars().any(|c| !c.is_ascii_hexdigit()) {
            return Err(eyre::eyre!(
                "account.validator_address is not a valid 20-byte hex address: {}",
                self.account.validator_address
            ));
        }
        if self.hyperlane.poll_interval_secs == 0 {
            return Err(eyre::eyre!("hyperlane.poll_interval_secs must be > 0"));
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
        let cfg: AttesterConfig = toml::from_str(MINIMAL).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.hyperlane.poll_interval_secs, 30);
        assert!(!cfg.hyperlane.gasless);
        assert!(cfg.health.is_none());
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut cfg: AttesterConfig = toml::from_str(MINIMAL).unwrap();
        cfg.hyperlane.poll_interval_secs = 0;
        assert!(cfg.validate().is_err());

        let mut cfg: AttesterConfig = toml::from_str(MINIMAL).unwrap();
        cfg.account.validator_address = "not-an-address".to_string();
        assert!(cfg.validate().is_err());

        assert!(toml::from_str::<AttesterConfig>(&format!(
            "{MINIMAL}\n[hyperlane]\ns3_endpoint = \"x\"\n"
        ))
        .is_err());
    }
}
