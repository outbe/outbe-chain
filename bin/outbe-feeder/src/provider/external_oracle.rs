//! External oracles read from their on-chain feed contracts: Chainlink Data
//! Feeds, RedStone push feeds, and any other vendor whose contract exposes
//! Chainlink's `AggregatorV3Interface` (`latestRoundData`, `decimals`,
//! `description`).
//!
//! Every `[[external_oracles]]` section is one provider instance whose `name`
//! is the provider name used in `currency_pairs.sources`, so one feeder can
//! hold the same market from several vendors as separate sources.
//!
//! Feeds are read with `eth_call` at the `latest` block. Vendors sign each
//! round, so reorg protection adds nothing; freshness comes from the round's
//! own `updatedAt`. Feeds carry no volume: the observation weighs one unit in
//! the aggregator.

use alloy_primitives::{Address, U256};
use alloy_sol_types::sol;
use async_trait::async_trait;
use eyre::{ensure, eyre, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use super::dex::math::scale_fp18;
use super::evm_rpc::Rpc;
use super::{checked_ticker, Provider, TickerPrice, VolumeInput};
use crate::config::FeederConfig;
use crate::fixed::FixedValue;

sol! {
    interface AggregatorV3 {
        function decimals() external view returns (uint8);
        function description() external view returns (string memory);
        function latestRoundData() external view returns (
            uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
    }
}

/// Longest documented heartbeat at Chainlink and RedStone is 24 hours; an
/// hour of slack covers relayer and inclusion delay. A round older than this
/// means the relayer stopped.
pub(crate) const MAX_FEED_AGE_SECS: u64 = 90_000;
const MAX_FUTURE_SECS: u64 = 60;

/// One provider instance: a vendor label, an EVM network and its feeds.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalOracleConfig {
    /// Provider name referenced by `currency_pairs.sources`, e.g. `chainlink`
    /// or `redstone_push`; must not collide with a built-in provider name.
    pub name: String,
    pub chain_id: u64,
    pub rpc_endpoint: String,
    pub feeds: Vec<FeedConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeedConfig {
    pub base: String,
    pub quote: String,
    /// Feed contract (the vendor's proxy address); it must implement
    /// Chainlink's `AggregatorV3Interface`. The implementation behind a proxy
    /// may rotate.
    pub contract: Address,
    /// Expected on-chain `description()`, e.g. `ETH / USD` (Chainlink) or
    /// `RedStone Price Feed for ETH`, guarding against a mistyped address.
    pub description: String,
}

impl FeedConfig {
    fn key(&self) -> String {
        format!("{}/{}", self.base, self.quote)
    }
}

impl ExternalOracleConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty() && !self.name.contains(char::is_whitespace),
            "external_oracle provider name must be a single non-empty word"
        );
        ensure!(
            self.chain_id > 0,
            "external_oracle chain_id must be positive"
        );
        let url = reqwest::Url::parse(&self.rpc_endpoint)
            .map_err(|_| eyre!("invalid external_oracle RPC URL"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "external_oracle RPC requires HTTP(S)"
        );
        ensure!(
            !self.feeds.is_empty(),
            "external_oracle provider has no feeds"
        );
        let mut keys = BTreeSet::new();
        let mut addresses = BTreeSet::new();
        for feed in &self.feeds {
            ensure!(
                !feed.base.trim().is_empty() && !feed.quote.trim().is_empty(),
                "external_oracle feed has an empty market asset"
            );
            ensure!(
                !feed.contract.is_zero(),
                "external_oracle feed {} has no contract address",
                feed.key()
            );
            ensure!(
                !feed.description.trim().is_empty(),
                "external_oracle feed {} has no description",
                feed.key()
            );
            ensure!(
                keys.insert(feed.key()),
                "duplicate feed {} in external_oracle provider {}",
                feed.key(),
                self.name
            );
            ensure!(
                addresses.insert(feed.contract),
                "contract {} configured twice in external_oracle provider {}",
                feed.contract,
                self.name
            );
        }
        Ok(())
    }
}

/// Validates `[[external_oracles]]` sections and the sources that
/// reference them by name. Built-in provider names are reserved.
pub(crate) fn validate_config(config: &FeederConfig, builtin: &[&str]) -> Result<()> {
    let mut names = BTreeSet::new();
    for section in &config.external_oracles {
        section.validate()?;
        ensure!(
            !builtin.contains(&section.name.as_str()),
            "external_oracle provider name '{}' is a built-in provider",
            section.name
        );
        ensure!(
            names.insert(section.name.as_str()),
            "duplicate external_oracle provider '{}'",
            section.name
        );
        ensure!(
            !config
                .provider_endpoints
                .iter()
                .any(|e| e.name == section.name),
            "configure external_oracle RPC in external_oracles, not provider_endpoints"
        );
    }
    for pair in &config.currency_pairs {
        for source in &pair.sources {
            let Some(section) = config
                .external_oracles
                .iter()
                .find(|s| s.name == source.provider)
            else {
                continue;
            };
            ensure!(
                section
                    .feeds
                    .iter()
                    .any(|f| f.base == source.base && f.quote == source.quote),
                "missing feed {}/{} in external_oracle provider {}",
                source.base,
                source.quote,
                section.name
            );
        }
    }
    Ok(())
}

/// Whether `name` is an `[[external_oracles]]` section.
pub(crate) fn is_section_name(config: &FeederConfig, name: &str) -> bool {
    config.external_oracles.iter().any(|s| s.name == name)
}

pub(crate) struct ExternalOracleProvider {
    name: String,
    rpc: Rpc,
    chain_id: u64,
    feeds: HashMap<String, FeedConfig>,
    /// Set once `eth_chainId` has been confirmed.
    chain_verified: AtomicBool,
    /// Feed key -> `decimals()`, cached after `description()` is verified.
    decimals: RwLock<HashMap<String, u8>>,
}

impl ExternalOracleProvider {
    pub fn new(section: &ExternalOracleConfig) -> Result<Self> {
        section.validate()?;
        Ok(Self {
            name: section.name.clone(),
            rpc: Rpc::new(&section.rpc_endpoint)?,
            chain_id: section.chain_id,
            feeds: section
                .feeds
                .iter()
                .map(|feed| (feed.key(), feed.clone()))
                .collect(),
            chain_verified: AtomicBool::new(false),
            decimals: RwLock::new(HashMap::new()),
        })
    }

    async fn verify_chain(&self) -> Result<()> {
        if self.chain_verified.load(Ordering::Relaxed) {
            return Ok(());
        }
        ensure!(
            self.rpc.chain_id().await? == self.chain_id,
            "RPC chain ID mismatch for chain {}",
            self.chain_id
        );
        self.chain_verified.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn feed_decimals(&self, feed: &FeedConfig) -> Result<u8> {
        if let Some(decimals) = self.decimals.read().await.get(&feed.key()) {
            return Ok(*decimals);
        }
        let description = self
            .rpc
            .call_latest(feed.contract, AggregatorV3::descriptionCall {})
            .await?;
        ensure!(
            description == feed.description,
            "feed {} description is {description:?}, expected {:?}",
            feed.key(),
            feed.description
        );
        let decimals = self
            .rpc
            .call_latest(feed.contract, AggregatorV3::decimalsCall {})
            .await?;
        ensure!(decimals <= 77, "unsupported feed decimals (>77)");
        self.decimals.write().await.insert(feed.key(), decimals);
        Ok(decimals)
    }

    async fn read_feed(&self, feed: &FeedConfig, now: u64) -> Result<FixedValue> {
        self.verify_chain().await?;
        let decimals = self.feed_decimals(feed).await?;
        let round = self
            .rpc
            .call_latest(feed.contract, AggregatorV3::latestRoundDataCall {})
            .await?;
        ensure!(round.answer.is_positive(), "feed answer is not positive");
        ensure!(
            round.updatedAt <= U256::from(now.saturating_add(MAX_FUTURE_SECS)),
            "feed updatedAt is in the future"
        );
        let age = U256::from(now).saturating_sub(round.updatedAt);
        ensure!(
            age <= U256::from(MAX_FEED_AGE_SECS),
            "feed round is stale ({age} s > {MAX_FEED_AGE_SECS} s)"
        );
        scale_fp18(round.answer.into_raw(), decimals)
    }
}

#[async_trait]
impl Provider for ExternalOracleProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut tickers = HashMap::new();
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        for (base, quote) in pairs {
            let key = format!("{base}/{quote}");
            let Some(feed) = self.feeds.get(&key) else {
                continue;
            };
            match self.read_feed(feed, now).await {
                Ok(price) => {
                    if let Some(ticker) = checked_ticker(
                        "external_oracle",
                        &key,
                        Some(price),
                        VolumeInput::Unavailable,
                    ) {
                        tickers.insert(key, ticker);
                    }
                }
                Err(error) => {
                    tracing::warn!(provider = %self.name, feed = %key, error = %error, "external_oracle feed skipped");
                }
            }
        }
        Ok(tickers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_server;
    use alloy_primitives::{address, Bytes, I256};
    use alloy_sol_types::SolValue;
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    };

    const AGGREGATOR: Address = address!("0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419");

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[derive(Default)]
    struct Fixture {
        wrong_chain: AtomicBool,
        wrong_description: AtomicBool,
        negative: AtomicBool,
        fail_round: AtomicBool,
        /// Seconds between the round's updatedAt and wall-clock time.
        age: AtomicI64,
    }

    impl Fixture {
        fn response(&self, request: &Value) -> Value {
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap();
            let params = &request["params"];
            let selector = |s: &str| alloy_primitives::keccak256(s).as_slice()[..4].to_vec();
            let result = match method {
                "eth_chainId" => json!(if self.wrong_chain.load(Ordering::Relaxed) {
                    "0x2"
                } else {
                    "0x1"
                }),
                "eth_call" => {
                    assert_eq!(params[1], "latest");
                    assert_eq!(params[0]["to"], json!(AGGREGATOR));
                    let data: Bytes = serde_json::from_value(params[0]["data"].clone()).unwrap();
                    let output = if data[..4] == selector("decimals()") {
                        U256::from(8u8).abi_encode()
                    } else if data[..4] == selector("description()") {
                        (if self.wrong_description.load(Ordering::Relaxed) {
                            "BTC / USD"
                        } else {
                            "ETH / USD"
                        })
                        .abi_encode()
                    } else if data[..4] == selector("latestRoundData()") {
                        if self.fail_round.load(Ordering::Relaxed) {
                            return json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"execution reverted"}});
                        }
                        let answer = if self.negative.load(Ordering::Relaxed) {
                            I256::MINUS_ONE
                        } else {
                            I256::try_from(250_000_000_000i64).unwrap() // 2500.00000000
                        };
                        let updated = (now() as i64 - self.age.load(Ordering::Relaxed)) as u64;
                        (
                            alloy_primitives::aliases::U80::from(1u8),
                            answer,
                            U256::from(updated),
                            U256::from(updated),
                            alloy_primitives::aliases::U80::from(1u8),
                        )
                            .abi_encode()
                    } else {
                        panic!("unexpected call")
                    };
                    json!(Bytes::from(output))
                }
                other => panic!("unexpected method {other}"),
            };
            json!({"jsonrpc":"2.0","id":id,"result":result})
        }
    }

    fn config(endpoint: &str) -> ExternalOracleConfig {
        ExternalOracleConfig {
            name: "chainlink".into(),
            chain_id: 1,
            rpc_endpoint: endpoint.to_owned(),
            feeds: vec![FeedConfig {
                base: "ETH".into(),
                quote: "840".into(),
                contract: AGGREGATOR,
                description: "ETH / USD".into(),
            }],
        }
    }

    async fn fresh_provider() -> (Arc<Fixture>, test_server::Server, ExternalOracleProvider) {
        let fixture = Arc::new(Fixture::default());
        let state = Arc::clone(&fixture);
        let server = test_server::start(Arc::new(move |request| state.response(request))).await;
        let provider = ExternalOracleProvider::new(&config(&server.endpoint)).unwrap();
        (fixture, server, provider)
    }

    fn eth_usd() -> Vec<(String, String)> {
        vec![("ETH".into(), "840".into())]
    }

    #[tokio::test]
    async fn fresh_round_is_normalized_to_fp18_without_volume() {
        let (_fixture, _server, provider) = fresh_provider().await;
        let tickers = provider.get_ticker_prices(&eth_usd()).await.unwrap();
        let ticker = &tickers["ETH/840"];
        assert_eq!(ticker.price, FixedValue::parse("2500").unwrap());
        assert!(ticker.volume.is_zero());
        // Unconfigured pairs are ignored, not errors.
        let none = provider
            .get_ticker_prices(&[("BTC".into(), "840".into())])
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn invalid_rounds_are_skipped_and_metadata_is_verified() {
        let (fixture, _server, provider) = fresh_provider().await;
        for flag in [&fixture.negative, &fixture.fail_round] {
            flag.store(true, Ordering::Relaxed);
            assert!(provider
                .get_ticker_prices(&eth_usd())
                .await
                .unwrap()
                .is_empty());
            flag.store(false, Ordering::Relaxed);
        }
        let max_age = i64::try_from(MAX_FEED_AGE_SECS).unwrap();
        fixture.age.store(max_age + 5, Ordering::Relaxed);
        assert!(provider
            .get_ticker_prices(&eth_usd())
            .await
            .unwrap()
            .is_empty());
        fixture.age.store(max_age - 5, Ordering::Relaxed);
        assert_eq!(
            provider.get_ticker_prices(&eth_usd()).await.unwrap().len(),
            1
        );
        fixture.age.store(-120, Ordering::Relaxed);
        assert!(provider
            .get_ticker_prices(&eth_usd())
            .await
            .unwrap()
            .is_empty());
        fixture.age.store(0, Ordering::Relaxed);

        // Chain id is verified once per network, before any feed read.
        let (fixture, _server, provider) = fresh_provider().await;
        fixture.wrong_chain.store(true, Ordering::Relaxed);
        assert!(provider
            .get_ticker_prices(&eth_usd())
            .await
            .unwrap()
            .is_empty());

        let (fixture, _server, provider) = fresh_provider().await;
        fixture.wrong_description.store(true, Ordering::Relaxed);
        assert!(provider
            .get_ticker_prices(&eth_usd())
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn config_validation_and_source_routing() {
        assert!(config("http://localhost:8545").validate().is_ok());
        let mut bad = config("http://localhost:8545");
        bad.feeds[0].description.clear();
        assert!(bad.validate().is_err());
        let mut bad = config("http://localhost:8545");
        bad.feeds.push(bad.feeds[0].clone());
        assert!(bad.validate().is_err(), "duplicate feed in one section");
        let mut bad = config("ws://localhost:8545");
        bad.feeds.clear();
        assert!(bad.validate().is_err());

        let mut feeder: FeederConfig = toml::from_str(
            r#"
            [chain]
            rpc_endpoint = "http://localhost:8545"
            chain_id = 1
            [account]
            private_key = "0x01"
            validator_address = "0x1111111111111111111111111111111111111111"
            [oracle]
            vote_period = 8
            [[currency_pairs]]
            base = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
            quote = "840"
            [[currency_pairs.sources]]
            provider = "chainlink"
            base = "ETH"
            quote = "840"
        "#,
        )
        .unwrap();
        assert!(
            feeder.validate().is_err(),
            "source without a matching section"
        );
        feeder
            .external_oracles
            .push(config("http://localhost:8545"));
        feeder.validate().unwrap();

        // The same market from a second vendor is a second source.
        let mut redstone = config("http://localhost:8545");
        redstone.name = "redstone_push".into();
        redstone.feeds[0].contract = address!("0x67F6838e58859d612E4ddF04dA396d6DABB66Dc4");
        redstone.feeds[0].description = "RedStone Price Feed for ETH".into();
        feeder.external_oracles.push(redstone);
        feeder.currency_pairs[0]
            .sources
            .push(crate::config::CurrencyPairSource {
                provider: "redstone_push".into(),
                base: "ETH".into(),
                quote: "840".into(),
            });
        feeder.validate().unwrap();
        let providers = crate::provider::create_providers(&feeder).unwrap();
        let mut names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
        names.sort_unstable();
        assert_eq!(names, ["chainlink", "redstone_push"]);

        feeder.external_oracles[1].name = "chainlink".into();
        assert!(feeder.validate().is_err(), "duplicate section name");
        feeder.external_oracles[1].name = "binance".into();
        assert!(feeder.validate().is_err(), "built-in name is reserved");
        feeder.external_oracles[1].name = "redstone_push".into();
        feeder.external_oracles[1].feeds[0].quote = "USD".into();
        assert!(feeder.validate().is_err(), "source without matching feed");
    }

    /// Live mainnet read of a Chainlink and a RedStone feed; run with
    /// `cargo test -p outbe-feeder live_mainnet -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_mainnet_feeds() {
        let mut cfg = config("https://ethereum-rpc.publicnode.com");
        cfg.feeds.push(FeedConfig {
            base: "USDC".into(),
            quote: "840".into(),
            contract: address!("0x8fFfFfd4AfB6115b954Bd326cbe7B4BA576818f6"),
            description: "USDC / USD".into(),
        });
        cfg.feeds.push(FeedConfig {
            base: "BTC".into(),
            quote: "840".into(),
            contract: address!("0xAB7f623fb2F6fea6601D4350FA0E2290663C28Fc"),
            description: "RedStone Price Feed for BTC".into(),
        });
        let provider = ExternalOracleProvider::new(&cfg).unwrap();
        let pairs = vec![
            ("ETH".into(), "840".into()),
            ("USDC".into(), "840".into()),
            ("BTC".into(), "840".into()),
        ];
        let tickers = provider.get_ticker_prices(&pairs).await.unwrap();
        for key in ["ETH/840", "USDC/840", "BTC/840"] {
            let price = tickers[key].price;
            eprintln!("{key}: {} FP18", price.raw());
            assert!(!price.is_zero());
        }
        let usdc = tickers["USDC/840"].price.raw();
        let one = FixedValue::parse("1").unwrap().raw();
        assert!(
            usdc.abs_diff(one) < one / U256::from(20u64),
            "USDC within 5% of 1"
        );
    }
}
