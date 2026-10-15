//! TOML configuration for the price-feeder daemon.

use alloy_primitives::{Address, U256};
use eyre::{Context, Result};
use outbe_primitives::stablecoin::{iso_4217_alpha, ISO_4217_NUMERIC_CODES};
use serde::Deserialize;
use std::path::Path;

use crate::fixed::FixedValue;
use outbe_primitives::asset_type::AssetType;
use outbe_primitives::units::SCALE_1E18;

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
    pub api_providers: Vec<ApiProviderConfig>,
    /// Finalized EVM pool readers. They are independent of the destination Outbe RPC.
    #[serde(default)]
    pub dex_providers: Vec<crate::provider::dex::DexProviderConfig>,
    /// Feeds read from another oracle's on-chain contract (Chainlink, RedStone
    /// push; any `AggregatorV3Interface` contract). One entry is one feed on
    /// one EVM network, under a name the operator chooses.
    #[serde(default)]
    pub onchain_providers: Vec<crate::provider::onchain_feed::OnchainFeedConfig>,
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
#[serde(deny_unknown_fields)]
pub struct CurrencyPairConfig {
    /// On-chain Oracle base asset: `COEN`, an ISO numeric code, or an address.
    pub base: String,
    /// On-chain Oracle quote asset: an ISO numeric code or an address.
    pub quote: String,
    /// Cross rate: `<base>/<quote>` = `<base>/<cross>` ÷ `<quote>/<cross>`. The
    /// markets then price `<quote>` in `<cross>`, and `<base>/<cross>` must be
    /// another configured pair.
    #[serde(default)]
    pub cross: Option<String>,
    /// External markets aggregated into this one Oracle observation.
    #[serde(rename = "markets", default)]
    pub sources: Vec<CurrencyPairSource>,
}

/// One provider-native market used to price an on-chain Oracle pair: the
/// provider's quote symbol and, when the provider calls the pair's base by
/// another name, that name.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CurrencyPairSource {
    pub provider: String,
    /// Empty means the pair's market base. See [`FeederConfig::resolve_sources`].
    #[serde(default)]
    pub base: String,
    pub quote: String,
}

/// The Oracle quote a provider quote feeds: `USDT` and `USDC` are the dollar;
/// an ISO 4217 code, numeric or alpha (`840`, `USD`, `978`, `EUR`), is that
/// currency.
pub fn oracle_quote_of(quote: &str) -> Option<String> {
    let quote = match quote {
        "USDT" | "USDC" => "840",
        other => other,
    };
    if quote.parse::<u16>().is_ok() {
        return Some(quote.to_owned());
    }
    let alpha: [u8; 3] = quote.as_bytes().try_into().ok()?;
    ISO_4217_NUMERIC_CODES
        .iter()
        .find(|code| iso_4217_alpha(**code) == Some(alpha))
        .map(|code| code.to_string())
}

/// Endpoint and access settings of one provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiProviderConfig {
    pub name: String,
    /// REST base URL. Empty selects the provider default.
    #[serde(default)]
    pub rest: String,
    /// Exchange market-stream endpoint. Empty selects the exchange default.
    #[serde(default)]
    pub websocket: String,
    /// Gateway key, one per validator. Only `redstone` takes one.
    #[serde(default)]
    pub api_key: String,
}

/// Deviation threshold for outlier filtering.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviationThreshold {
    pub base: String,
    /// Maximum standard deviations from median to accept.
    #[serde(default = "default_deviation")]
    pub threshold: FixedValue,
}

fn default_vote_period() -> u64 {
    2
}
fn default_poll_interval() -> u64 {
    2
}
fn default_deviation() -> FixedValue {
    FixedValue::from_raw(U256::from_limbs([2_000_000_000_000_000_000u64, 0, 0, 0]))
}

impl FeederConfig {
    /// Loads configuration from a TOML file. A top-level `pairs` key names a
    /// second file, relative to this one, whose sections join the config.
    pub fn load(path: &str) -> Result<Self> {
        let mut table = read_table(Path::new(path))?;
        if let Some(pairs) = table.remove("pairs") {
            let pairs = pairs
                .as_str()
                .ok_or_else(|| eyre::eyre!("`pairs` must be a file path"))?;
            let pairs_path = Path::new(path)
                .parent()
                .unwrap_or(Path::new(""))
                .join(pairs);
            for (key, value) in read_table(&pairs_path)? {
                if table.insert(key.clone(), value).is_some() {
                    return Err(eyre::eyre!(
                        "`{key}` is set in both {path} and {}",
                        pairs_path.display()
                    ));
                }
            }
        }
        let mut config: Self = table
            .try_into()
            .with_context(|| "failed to parse feeder config")?;
        config.resolve_sources()?;
        Ok(config)
    }

    /// Fills each market's default base and hands the DEX and on-chain
    /// providers the markets that name them.
    pub fn resolve_sources(&mut self) -> Result<()> {
        for pair in &mut self.currency_pairs {
            let base = pair.market_base().to_owned();
            for source in &mut pair.sources {
                if source.base.is_empty() {
                    source.base = base.clone();
                }
            }
        }
        crate::provider::dex::collect_markets(self);
        crate::provider::onchain_feed::collect_feeds(self);
        Ok(())
    }

    /// Known provider names.
    const KNOWN_PROVIDERS: &'static [&'static str] = &[
        "mock",
        "pyth",
        "binance",
        "kraken",
        "okx",
        "gate",
        "huobi",
        "mexc",
        "coinbase",
        "mock_http",
        "redstone",
        "uniswap",
        "pancakeswap",
    ];

    /// Validates configuration at startup. Returns error for invalid values.
    pub fn validate(&self) -> Result<()> {
        self.validate_oracle_and_account()?;
        self.validate_currency_pairs()?;
        self.validate_api_providers()?;
        crate::provider::dex::validate_config(self)?;
        crate::provider::onchain_feed::validate_config(self, Self::KNOWN_PROVIDERS)
    }

    fn validate_oracle_and_account(&self) -> Result<()> {
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
        Ok(())
    }

    /// Every on-chain pair is distinct, well-formed and has at least one
    /// known, distinct source market.
    fn validate_currency_pairs(&self) -> Result<()> {
        let mut oracle_pairs = std::collections::BTreeSet::new();
        for pair in &self.currency_pairs {
            let base = parse_oracle_asset(&pair.base)?;
            let quote = parse_oracle_asset(&pair.quote)?;
            if base == quote {
                return Err(eyre::eyre!(
                    "Oracle pair {}/{} uses the same asset twice",
                    pair.base,
                    pair.quote
                ));
            }
            if matches!(AssetType::from(base), AssetType::IsoCurrency(_))
                && matches!(AssetType::from(quote), AssetType::Native)
            {
                return Err(eyre::eyre!(
                    "ISO/COEN pair {}/{} is invalid; configure COEN/ISO",
                    pair.base,
                    pair.quote
                ));
            }
            let unordered_key = if base < quote {
                (base, quote)
            } else {
                (quote, base)
            };
            if !oracle_pairs.insert(unordered_key) {
                return Err(eyre::eyre!(
                    "duplicate or inverse Oracle pair {}/{}",
                    pair.base,
                    pair.quote
                ));
            }
            self.validate_cross(pair)?;
            self.validate_pair_sources(pair)?;
        }
        Ok(())
    }

    /// A cross pair is `<asset>/<ISO>` over another ISO code whose
    /// `<asset>/<cross>` pair is configured directly, not as a cross itself.
    fn validate_cross(&self, pair: &CurrencyPairConfig) -> Result<()> {
        let Some(cross) = &pair.cross else {
            return Ok(());
        };
        let iso = |code: &str| code.parse::<u16>().is_ok_and(|c| (1..=999).contains(&c));
        if iso(&pair.base) || !iso(&pair.quote) || !iso(cross) {
            return Err(eyre::eyre!(
                "cross pair {}/{} with cross '{cross}' needs a COEN or token base, an ISO quote and an ISO cross",
                pair.base,
                pair.quote
            ));
        }
        if self.cross_leg(pair).is_none() {
            return Err(eyre::eyre!(
                "cross pair {}/{} needs a direct {}/{cross} pair",
                pair.base,
                pair.quote,
                pair.base
            ));
        }
        Ok(())
    }

    /// The direct `<base>/<cross>` pair a cross pair divides.
    pub(crate) fn cross_leg(&self, pair: &CurrencyPairConfig) -> Option<&CurrencyPairConfig> {
        let cross = pair.cross.as_deref()?;
        let base = parse_oracle_asset(&pair.base).ok()?;
        self.currency_pairs.iter().find(|leg| {
            leg.cross.is_none()
                && leg.quote == cross
                && parse_oracle_asset(&leg.base).is_ok_and(|leg_base| leg_base == base)
        })
    }

    /// A pair names at least one source; each source is well-formed, uses a
    /// known provider and appears once.
    fn validate_pair_sources(&self, pair: &CurrencyPairConfig) -> Result<()> {
        if pair.sources.is_empty() {
            return Err(eyre::eyre!(
                "currency pair {}/{} has no sources configured",
                pair.base,
                pair.quote
            ));
        }
        crate::provider::validate_currency_rate_sources(pair)?;
        let mut sources = std::collections::BTreeSet::new();
        for source in &pair.sources {
            crate::provider::cex::validate_exchange_market(source)?;
            // A market priced in another currency than the pair would vote a
            // wrong price. Non-ISO quotes (token addresses) are not checked.
            let quote = pair.market_quote();
            if quote.parse::<u16>().is_ok()
                && oracle_quote_of(&source.quote).as_deref() != Some(quote)
            {
                return Err(eyre::eyre!(
                    "provider '{}' market quote '{}' does not price pair {}/{}",
                    source.provider,
                    source.quote,
                    pair.base,
                    pair.quote
                ));
            }
            if !Self::KNOWN_PROVIDERS.contains(&source.provider.as_str())
                && !crate::provider::onchain_feed::is_section_name(self, &source.provider)
            {
                return Err(eyre::eyre!(
                    "unknown provider '{}' for pair {}/{}. Known: {:?} or an onchain_providers entry name",
                    source.provider,
                    pair.base,
                    pair.quote,
                    Self::KNOWN_PROVIDERS
                ));
            }
            if !sources.insert((
                source.provider.as_str(),
                source.base.as_str(),
                source.quote.as_str(),
            )) {
                return Err(eyre::eyre!(
                    "duplicate provider source '{}' {}/{} for pair {}/{}",
                    source.provider,
                    source.base,
                    source.quote,
                    pair.base,
                    pair.quote
                ));
            }
        }
        Ok(())
    }

    /// Endpoint names are known and unique; websocket overrides are only
    /// accepted for streaming exchanges and look like a websocket address.
    fn validate_api_providers(&self) -> Result<()> {
        let mut endpoint_names = std::collections::BTreeSet::new();
        for endpoint in &self.api_providers {
            if !Self::KNOWN_PROVIDERS.contains(&endpoint.name.as_str()) {
                return Err(eyre::eyre!(
                    "unknown api provider '{}'. Known: {:?}",
                    endpoint.name,
                    Self::KNOWN_PROVIDERS
                ));
            }
            if !endpoint_names.insert(endpoint.name.as_str()) {
                return Err(eyre::eyre!("duplicate api provider '{}'", endpoint.name));
            }
            if !endpoint.api_key.is_empty()
                && !matches!(endpoint.name.as_str(), "redstone" | "pyth")
            {
                return Err(eyre::eyre!("provider '{}' takes no api_key", endpoint.name));
            }
            if !endpoint.websocket.is_empty() {
                Self::validate_websocket_endpoint(endpoint)?;
            }
        }
        Ok(())
    }

    /// Only an exchange takes a websocket override, and it must be a `ws://`
    /// or `wss://` URL or a bare host.
    fn validate_websocket_endpoint(endpoint: &ApiProviderConfig) -> Result<()> {
        if crate::provider::cex::ExchangeKind::for_name(&endpoint.name).is_none() {
            return Err(eyre::eyre!(
                "provider '{}' does not support websocket market streams",
                endpoint.name
            ));
        }
        let websocket = endpoint.websocket.trim();
        if websocket.is_empty()
            || websocket.contains(char::is_whitespace)
            || (websocket.contains("://")
                && !websocket.starts_with("ws://")
                && !websocket.starts_with("wss://"))
        {
            return Err(eyre::eyre!(
                "provider '{}' has invalid websocket endpoint '{}'",
                endpoint.name,
                endpoint.websocket
            ));
        }
        Ok(())
    }

    /// Returns the deviation threshold for a given base asset, or the default.
    pub fn deviation_for(&self, base: &str) -> FixedValue {
        self.deviation_thresholds
            .iter()
            .find(|d| d.base == base)
            .map(|d| d.threshold)
            .unwrap_or_else(|| FixedValue::from_raw(U256::from(2u64) * SCALE_1E18))
    }
}

impl CurrencyPairConfig {
    pub(crate) fn oracle_pair(&self) -> Result<(Address, Address)> {
        Ok((
            parse_oracle_asset(&self.base)?,
            parse_oracle_asset(&self.quote)?,
        ))
    }

    /// The asset the markets price: the quote currency for a cross pair.
    pub(crate) fn market_base(&self) -> &str {
        if self.cross.is_some() {
            &self.quote
        } else {
            &self.base
        }
    }

    /// The currency the markets price in: the cross currency for a cross pair.
    pub(crate) fn market_quote(&self) -> &str {
        self.cross.as_deref().unwrap_or(&self.quote)
    }
}

fn read_table(path: &Path) -> Result<toml::Table> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse config file: {}", path.display()))
}

fn parse_oracle_asset(symbol: &str) -> Result<Address> {
    let text = symbol.trim();
    if text.eq_ignore_ascii_case("COEN") {
        return Ok(Address::ZERO);
    }
    if let Ok(code) = text.parse::<u16>() {
        if (1..=999).contains(&code) {
            return Ok(AssetType::IsoCurrency(code).into());
        }
    }
    text.parse::<Address>()
        .map_err(|_| eyre::eyre!("unknown on-chain Oracle asset '{symbol}'"))
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
            api_providers: vec![],
            dex_providers: vec![],
            onchain_providers: vec![],
            health: None,
        }
    }

    fn source(provider: &str, base: &str, quote: &str) -> CurrencyPairSource {
        CurrencyPairSource {
            provider: provider.to_owned(),
            base: base.to_owned(),
            quote: quote.to_owned(),
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
    fn test_validate_rejects_empty_sources() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "840".to_string(),
            cross: None,
            sources: vec![],
        });
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_unknown_provider() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "840".to_string(),
            cross: None,
            sources: vec![source("nonexistent_exchange", "COEN", "USDT")],
        });
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_known_provider() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "840".to_string(),
            cross: None,
            sources: vec![source("mock", "COEN", "USDT")],
        });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn currency_pair_rejects_removed_chain_denom() {
        let result = toml::from_str::<CurrencyPairConfig>(
            "base = 'COEN'\nquote = '840'\nchain_denom = 'unit'\n[[sources]]\nprovider = 'mock'\nbase = 'COEN'\nquote = 'USDT'",
        );
        assert!(result.is_err());
    }

    #[test]
    fn currency_pair_rejects_the_removed_providers_array() {
        let result = toml::from_str::<CurrencyPairConfig>(
            "base = 'COEN'\nquote = '840'\nproviders = ['mock']",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_accepts_mock_http_provider() {
        let mut cfg = minimal_config(2);
        cfg.api_providers.push(ApiProviderConfig {
            name: "mock_http".to_string(),
            rest: "http://localhost:8000".to_string(),
            websocket: String::new(),
            api_key: String::new(),
        });
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "840".to_string(),
            cross: None,
            sources: vec![source("mock_http", "COEN", "USDT")],
        });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_exchange_websocket_endpoint() {
        let mut cfg = minimal_config(2);
        cfg.api_providers.push(ApiProviderConfig {
            name: "binance".to_string(),
            rest: String::new(),
            websocket: "wss://stream.binance.com:9443/ws".to_string(),
            api_key: String::new(),
        });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_websocket_for_rest_only_provider() {
        let mut cfg = minimal_config(2);
        cfg.api_providers.push(ApiProviderConfig {
            name: "mock_http".to_string(),
            rest: "http://localhost:8000".to_string(),
            websocket: "ws://localhost:8001".to_string(),
            api_key: String::new(),
        });
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("does not support websocket"));
    }

    const HEADER: &str = r#"
        [chain]
        rpc_endpoint = "http://localhost:8545"
        chain_id = 1
        [account]
        private_key = "0x01"
        validator_address = "0x1111111111111111111111111111111111111111"
        [oracle]
        vote_period = 8
    "#;

    fn parse(pairs: &str) -> FeederConfig {
        let mut cfg: FeederConfig = toml::from_str(&format!("{HEADER}{pairs}")).unwrap();
        cfg.resolve_sources().unwrap();
        cfg
    }

    #[test]
    fn cross_pair_markets_price_the_currency_and_need_the_coen_leg() {
        let cross = r#"
            [[currency_pairs]]
            base = "COEN"
            quote = "978"
            cross = "840"
            markets = [{ provider = "pyth", quote = "840" }]
        "#;
        let leg = r#"
            [[currency_pairs]]
            base = "COEN"
            quote = "840"
            markets = [{ provider = "binance", quote = "USDT" }]
        "#;
        let mut cfg = parse(&format!("{leg}{cross}"));
        assert_eq!(
            cfg.currency_pairs[1].sources,
            vec![source("pyth", "978", "840")]
        );
        cfg.validate().unwrap();

        cfg.currency_pairs[1].sources[0].provider = "binance".into();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("not for cross-currency rates"), "{err}");

        cfg.currency_pairs[1].sources[0] = source("pyth", "978", "978");
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("does not price pair"), "{err}");

        let err = parse(cross).validate().unwrap_err().to_string();
        assert!(err.contains("needs a direct COEN/840 pair"), "{err}");
    }

    #[test]
    fn fx_pair_refuses_exchanges_and_dex() {
        let mut cfg = parse(
            r#"
            [[currency_pairs]]
            base = "978"
            quote = "840"
            markets = [{ provider = "kraken", base = "EUR", quote = "USD" }]
        "#,
        );
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("not for cross-currency rates"), "{error}");

        cfg.currency_pairs[0].sources[0].provider = "redstone".into();
        cfg.api_providers
            .push(toml::from_str("name = 'redstone'\napi_key = 'key'").unwrap());
        cfg.validate().unwrap();
    }

    #[test]
    fn an_exchange_market_is_not_quoted_in_an_iso_code() {
        let mut cfg = minimal_config(8);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "COEN".to_string(),
            quote: "840".to_string(),
            cross: None,
            sources: vec![source("binance", "COEN", "840")],
        });
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("ISO code"), "{error}");

        cfg.currency_pairs[0].sources[0].quote = "USDT".into();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn only_redstone_takes_an_api_key() {
        let mut cfg = minimal_config(8);
        cfg.api_providers.push(ApiProviderConfig {
            name: "binance".to_string(),
            rest: String::new(),
            websocket: String::new(),
            api_key: "key".to_string(),
        });
        assert!(cfg.validate().is_err());

        cfg.api_providers[0].name = "redstone".to_string();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_duplicate_api_providers() {
        let mut cfg = minimal_config(2);
        for _ in 0..2 {
            cfg.api_providers.push(ApiProviderConfig {
                name: "binance".to_string(),
                rest: String::new(),
                websocket: String::new(),
                api_key: String::new(),
            });
        }
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate api provider"));
    }

    #[test]
    fn test_validate_rejects_iso_coen_but_preserves_generic_orientation() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "840".to_string(),
            quote: "COEN".to_string(),
            cross: None,
            sources: vec![source("mock", "USDT", "COEN")],
        });
        assert!(cfg.validate().unwrap_err().to_string().contains("COEN/ISO"));

        cfg.currency_pairs[0].base = "999".to_string();
        assert!(cfg.validate().unwrap_err().to_string().contains("COEN/ISO"));

        cfg.currency_pairs[0].base = "840".to_string();
        cfg.currency_pairs[0].quote = "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599".to_string();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_an_unknown_on_chain_symbol() {
        let mut cfg = minimal_config(2);
        cfg.currency_pairs.push(CurrencyPairConfig {
            base: "BTC".to_string(),
            quote: "840".to_string(),
            cross: None,
            sources: vec![source("binance", "BTC", "USDT")],
        });

        let err = cfg.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("unknown on-chain Oracle asset 'BTC'"));
    }

    #[test]
    fn each_pair_lists_its_own_markets() {
        let mut cfg = parse(
            r#"
            [[currency_pairs]]
            base = "COEN"
            quote = "840"
            markets = [
              { provider = "binance", quote = "USDT" },
              { provider = "coinbase", quote = "USD" },
            ]
            [[currency_pairs]]
            base = "COEN"
            quote = "978"
            markets = [{ provider = "kraken", quote = "EUR" }]
            [[currency_pairs]]
            base = "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599"
            quote = "840"
            markets = [{ provider = "binance", base = "BTC", quote = "USDT" }]
        "#,
        );
        cfg.validate().unwrap();

        // The base defaults to the pair's base. Two pairs share 840 freely.
        assert_eq!(
            cfg.currency_pairs[0].sources,
            vec![
                source("binance", "COEN", "USDT"),
                source("coinbase", "COEN", "USD")
            ]
        );
        assert_eq!(
            cfg.currency_pairs[1].sources,
            vec![source("kraken", "COEN", "EUR")]
        );
        assert_eq!(
            cfg.currency_pairs[2].sources,
            vec![source("binance", "BTC", "USDT")]
        );
        assert_eq!(
            cfg.currency_pairs[0].oracle_pair().unwrap().0,
            Address::ZERO
        );
        assert_eq!(
            AssetType::from(cfg.currency_pairs[0].oracle_pair().unwrap().1),
            AssetType::IsoCurrency(840)
        );

        // A market quoted in another currency than its pair is rejected.
        cfg.currency_pairs[1].sources[0].quote = "USD".into();
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("does not price pair COEN/978"), "{error}");
        cfg.currency_pairs[1].sources[0].quote = "EUR".into();

        // A pair without markets is rejected.
        cfg.currency_pairs[1].sources.clear();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn deviation_threshold_requires_an_exact_decimal_string() {
        let quoted: DeviationThreshold =
            toml::from_str("base = 'ETH'\nthreshold = '2.000000000000000001'").unwrap();
        assert_eq!(
            quoted.threshold.raw(),
            U256::from(2u64) * SCALE_1E18 + U256::ONE
        );

        let numeric = toml::from_str::<DeviationThreshold>("base = 'ETH'\nthreshold = 2.0");
        assert!(numeric.is_err());
    }

    /// `pairs` names a second file, relative to the main one, and a section
    /// may not appear in both.
    #[test]
    fn pairs_file_joins_the_main_config() {
        let directory =
            std::env::temp_dir().join(format!("outbe-feeder-pairs-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let main = directory.join("feeder.toml");
        std::fs::write(&main, format!("pairs = \"pairs.toml\"\n{HEADER}")).unwrap();
        let pairs = "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\n\
                     markets = [{ provider = \"mock\", quote = \"USDT\" }]\n";
        std::fs::write(directory.join("pairs.toml"), pairs).unwrap();
        let cfg = FeederConfig::load(main.to_str().unwrap()).unwrap();
        cfg.validate().unwrap();
        assert_eq!(
            cfg.currency_pairs[0].sources,
            vec![source("mock", "COEN", "USDT")]
        );

        std::fs::write(directory.join("pairs.toml"), format!("{pairs}[oracle]\n")).unwrap();
        let error = FeederConfig::load(main.to_str().unwrap()).unwrap_err();
        assert!(
            error.to_string().contains("`oracle` is set in both"),
            "{error}"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn test_price_oracle_script_config_loads() {
        let path = format!(
            "{}/../../scripts/price-oracle/config.toml",
            env!("CARGO_MANIFEST_DIR")
        );
        let cfg = FeederConfig::load(&path).unwrap();
        // Canonical localnet chain id (scripts/prepare_network.py DEFAULT_CHAIN_ID).
        assert_eq!(cfg.chain.chain_id, 54322345);
        assert_eq!(cfg.oracle.vote_period, 8);
        // COEN/840 is the only pair the chain registers. The decorative
        // XAU/BTC/ETH/stablecoin entries were dropped because no code read them
        // and they name assets with no on-chain address.
        assert_eq!(cfg.currency_pairs.len(), 1);
        assert_eq!(cfg.currency_pairs[0].base, "COEN");
        assert_eq!(cfg.currency_pairs[0].quote, "840");
        assert!(cfg
            .currency_pairs
            .iter()
            .all(|pair| { pair.sources == vec![source("mock_http", "COEN", "840")] }));
        assert!(cfg.validate().is_ok());
    }
}
