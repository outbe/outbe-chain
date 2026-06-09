//! TOML configuration for the price-feeder daemon.

use eyre::{Context, Result};
use serde::Deserialize;

/// Top-level feeder configuration.
#[derive(Debug, Deserialize)]
pub struct FeederConfig {
    pub chain: ChainConfig,
    pub account: AccountConfig,
    pub oracle: OracleConfig,
    #[serde(default)]
    pub currency_pairs: Vec<CurrencyPairConfig>,
    #[serde(default)]
    pub deviation_thresholds: Vec<DeviationThreshold>,
    #[serde(default)]
    pub provider_endpoints: Vec<ProviderEndpointConfig>,
    /// Health/status HTTP server configuration.
    pub health: Option<HealthConfig>,
}

/// Health server settings.
#[derive(Debug, Deserialize)]
pub struct HealthConfig {
    /// Enable health server (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Bind address (default: 0.0.0.0:9002).
    #[serde(default = "default_health_bind")]
    pub bind_address: String,
}

fn default_true() -> bool {
    true
}

fn default_health_bind() -> String {
    "0.0.0.0:9002".to_string()
}

/// Chain connection settings.
#[derive(Debug, Deserialize)]
pub struct ChainConfig {
    /// JSON-RPC endpoint (HTTP).
    pub rpc_endpoint: String,
    /// Chain ID.
    pub chain_id: u64,
    /// Submit validator oracle votes through the guarded ZeroFee txpool policy.
    #[serde(default)]
    pub gasless_oracle_votes: bool,
}

/// Feeder account credentials.
#[derive(Debug, Deserialize)]
pub struct AccountConfig {
    /// Hex-encoded private key of the feeder account.
    /// In production, this should be loaded from a keystore file instead.
    pub private_key: String,
    /// Validator address this feeder acts on behalf of.
    pub validator_address: String,
}

/// Oracle-specific settings.
#[derive(Debug, Deserialize)]
pub struct OracleConfig {
    /// Vote period in blocks (must match on-chain config).
    #[serde(default = "default_vote_period")]
    pub vote_period: u64,
    /// How often to poll for new blocks (seconds).
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

/// A currency pair to feed prices for.
#[derive(Debug, Clone, Deserialize)]
pub struct CurrencyPairConfig {
    pub base: String,
    pub quote: String,
    /// Cosmos/test config compatibility. The Reth feeder signs pair hashes from
    /// base/quote and does not need the bank denom while submitting votes.
    #[serde(default)]
    #[allow(dead_code)]
    pub chain_denom: Option<String>,
    pub providers: Vec<String>,
}

/// Optional REST/WebSocket endpoint override for a provider.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderEndpointConfig {
    pub name: String,
    #[serde(default)]
    pub rest: String,
    /// Retained for Cosmos/test config compatibility. The current Rust
    /// `mock_http` provider uses REST only.
    #[serde(default)]
    #[allow(dead_code)]
    pub websocket: String,
}

/// Deviation threshold for outlier filtering.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviationThreshold {
    pub base: String,
    /// Maximum standard deviations from median to accept.
    #[serde(default = "default_deviation")]
    pub threshold: f64,
}

fn default_vote_period() -> u64 {
    2
}
fn default_poll_interval() -> u64 {
    2
}
fn default_deviation() -> f64 {
    2.0
}

impl FeederConfig {
    /// Loads configuration from a TOML file.
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {path}"))?;
        toml::from_str(&content).with_context(|| "failed to parse feeder config")
    }

    /// Known provider names.
    const KNOWN_PROVIDERS: &'static [&'static str] = &[
        "mock",
        "pyth",
        "chainlink",
        "binance",
        "kraken",
        "okx",
        "gate",
        "huobi",
        "mexc",
        "coinbase",
        "mock_http",
    ];

    /// Validates configuration at startup. Returns error for invalid values.
    pub fn validate(&self) -> Result<()> {
        // vote_period must be > 0
        if self.oracle.vote_period == 0 {
            return Err(eyre::eyre!(
                "oracle.vote_period must be > 0, got 0 (would cause division by zero)"
            ));
        }

        // validator_address must parse as a hex address
        let addr = self.account.validator_address.trim_start_matches("0x");
        if addr.len() != 40 || addr.chars().any(|c| !c.is_ascii_hexdigit()) {
            return Err(eyre::eyre!(
                "account.validator_address is not a valid 20-byte hex address: {}",
                self.account.validator_address
            ));
        }

        // Each pair must have at least 1 provider
        for pair in &self.currency_pairs {
            if pair.providers.is_empty() {
                return Err(eyre::eyre!(
                    "currency pair {}/{} has no providers configured",
                    pair.base,
                    pair.quote
                ));
            }
            // All provider names must be known
            for provider in &pair.providers {
                if !Self::KNOWN_PROVIDERS.contains(&provider.as_str()) {
                    return Err(eyre::eyre!(
                        "unknown provider '{}' for pair {}/{}. Known: {:?}",
                        provider,
                        pair.base,
                        pair.quote,
                        Self::KNOWN_PROVIDERS
                    ));
                }
            }
        }

        for endpoint in &self.provider_endpoints {
            if !Self::KNOWN_PROVIDERS.contains(&endpoint.name.as_str()) {
                return Err(eyre::eyre!(
                    "unknown provider endpoint '{}'. Known: {:?}",
                    endpoint.name,
                    Self::KNOWN_PROVIDERS
                ));
            }
        }

        Ok(())
    }

    /// Returns the deviation threshold for a given base asset, or the default.
    pub fn deviation_for(&self, base: &str) -> f64 {
        self.deviation_thresholds
            .iter()
            .find(|d| d.base == base)
            .map(|d| d.threshold)
            .unwrap_or(2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config(vote_period: u64) -> FeederConfig {
        FeederConfig {
            chain: ChainConfig {
                rpc_endpoint: "http://localhost:8545".to_string(),
                chain_id: 1,
                gasless_oracle_votes: false,
            },
            account: AccountConfig {
                private_key: "0xdead".to_string(),
                validator_address: "0x1111111111111111111111111111111111111111".to_string(),
            },
            oracle: OracleConfig {
                vote_period,
                poll_interval_secs: 2,
            },
            currency_pairs: vec![],
            deviation_thresholds: vec![],
            provider_endpoints: vec![],
            health: None,
        }
    }

    #[test]
    fn test_validate_rejects_zero_vote_period() {
        let cfg = minimal_config(0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_vote_period_zero() {
        let err = minimal_config(0).validate().unwrap_err();
        assert!(err.to_string().contains("vote_period must be > 0"));
    }

    #[test]
    fn test_validate_accepts_valid_vote_period() {
        let cfg = minimal_config(2);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_invalid_validator_address() {
        let mut cfg = minimal_config(2);
        cfg.account.validator_address = "not-an-address".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_empty_providers() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "0xUSD".to_string(),
            chain_denom: None,
            providers: vec![],
        });
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_unknown_provider() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "0xUSD".to_string(),
            chain_denom: None,
            providers: vec!["nonexistent_exchange".to_string()],
        });
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_known_provider() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "0xUSD".to_string(),
            chain_denom: None,
            providers: vec!["mock".to_string()],
        });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_mock_http_provider() {
        let mut cfg = minimal_config(2);
        cfg.provider_endpoints.push(ProviderEndpointConfig {
            name: "mock_http".to_string(),
            rest: "http://localhost:8000".to_string(),
            websocket: "localhost:8000".to_string(),
        });
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "0xUSD".to_string(),
            chain_denom: Some("unit".to_string()),
            providers: vec!["mock_http".to_string()],
        });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_price_oracle_script_config_loads() {
        let path = format!(
            "{}/../../scripts/price-oracle/config.toml",
            env!("CARGO_MANIFEST_DIR")
        );
        let cfg = FeederConfig::load(&path).unwrap();
        assert_eq!(cfg.chain.chain_id, 512215);
        assert_eq!(cfg.oracle.vote_period, 8);
        assert_eq!(cfg.currency_pairs.len(), 7);
        assert!(cfg
            .currency_pairs
            .iter()
            .all(|pair| pair.providers == vec!["mock_http".to_string()]));
        assert!(cfg.validate().is_ok());
    }
}
