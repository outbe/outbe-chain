//! Chainlink Data Feeds provider.
//!
//! Reads `AggregatorV3Interface` contracts over EVM JSON-RPC at the `latest`
//! block. Rounds are signed by the Chainlink DON, so reorg protection adds
//! nothing; freshness is enforced from the round's own `updatedAt`. Feeds
//! carry no volume: the observation weighs one unit in the aggregator.

use alloy_primitives::{aliases::U1024, Address, U256};
use alloy_sol_types::sol;
use async_trait::async_trait;
use eyre::{ensure, eyre, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use tokio::sync::RwLock;

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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChainlinkProviderConfig {
    pub chain_id: u64,
    pub rpc_endpoint: String,
    pub feeds: Vec<FeedConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeedConfig {
    pub base: String,
    pub quote: String,
    /// Proxy address from docs.chain.link; the aggregator behind it may rotate.
    pub aggregator: Address,
    /// Expected on-chain `description()`, e.g. `ETH / USD`, guarding against a
    /// mistyped address.
    pub description: String,
    /// Maximum round age: the feed's documented heartbeat plus slack.
    pub max_age_secs: u64,
}

impl FeedConfig {
    fn key(&self) -> String {
        format!("{}/{}", self.base, self.quote)
    }
}

impl ChainlinkProviderConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.chain_id > 0, "chainlink chain_id must be positive");
        let url = reqwest::Url::parse(&self.rpc_endpoint)
            .map_err(|_| eyre!("invalid chainlink RPC URL"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "chainlink RPC requires HTTP(S)"
        );
        ensure!(!self.feeds.is_empty(), "chainlink provider has no feeds");
        let mut keys = BTreeSet::new();
        let mut addresses = BTreeSet::new();
        for feed in &self.feeds {
            ensure!(
                !feed.base.trim().is_empty() && !feed.quote.trim().is_empty(),
                "chainlink feed has an empty market asset"
            );
            ensure!(
                !feed.aggregator.is_zero(),
                "chainlink feed {} has no aggregator address",
                feed.key()
            );
            ensure!(
                !feed.description.trim().is_empty(),
                "chainlink feed {} has no description",
                feed.key()
            );
            ensure!(
                (1..=7 * 86_400).contains(&feed.max_age_secs),
                "chainlink feed {} max_age_secs must be 1..=604800",
                feed.key()
            );
            ensure!(
                keys.insert(feed.key()),
                "duplicate chainlink feed {}",
                feed.key()
            );
            ensure!(
                addresses.insert(feed.aggregator),
                "chainlink aggregator {} configured more than once",
                feed.aggregator
            );
        }
        Ok(())
    }
}

/// Cross-checks `chainlink` sources against the `[[chainlink_providers]]` section.
pub(crate) fn validate_config(config: &FeederConfig) -> Result<()> {
    ensure!(
        config.chainlink_providers.len() <= 1,
        "at most one [[chainlink_providers]] entry is supported"
    );
    for chainlink in &config.chainlink_providers {
        chainlink.validate()?;
    }
    ensure!(
        !config
            .provider_endpoints
            .iter()
            .any(|e| e.name == "chainlink"),
        "configure chainlink RPC in chainlink_providers, not provider_endpoints"
    );
    for pair in &config.currency_pairs {
        for source in pair.sources.iter().filter(|s| s.provider == "chainlink") {
            let chainlink = config
                .chainlink_providers
                .first()
                .ok_or_else(|| eyre!("missing chainlink configuration"))?;
            ensure!(
                chainlink
                    .feeds
                    .iter()
                    .any(|f| f.base == source.base && f.quote == source.quote),
                "missing chainlink feed {}/{}",
                source.base,
                source.quote
            );
        }
    }
    Ok(())
}

/// Verified once per feed: aggregator identity and scale.
#[derive(Clone, Copy)]
struct FeedMeta {
    decimals: u8,
}

pub(crate) struct ChainlinkProvider {
    rpc: Rpc,
    chain_id: u64,
    feeds: HashMap<String, FeedConfig>,
    meta: RwLock<HashMap<String, FeedMeta>>,
}

impl ChainlinkProvider {
    pub fn new(config: &ChainlinkProviderConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            rpc: Rpc::new(&config.rpc_endpoint)?,
            chain_id: config.chain_id,
            feeds: config
                .feeds
                .iter()
                .map(|feed| (feed.key(), feed.clone()))
                .collect(),
            meta: RwLock::new(HashMap::new()),
        })
    }

    async fn feed_meta(&self, feed: &FeedConfig) -> Result<FeedMeta> {
        if let Some(meta) = self.meta.read().await.get(&feed.key()) {
            return Ok(*meta);
        }
        let description = self
            .rpc
            .call_latest(feed.aggregator, AggregatorV3::descriptionCall {})
            .await?;
        ensure!(
            description == feed.description,
            "chainlink feed {} description is {description:?}, expected {:?}",
            feed.key(),
            feed.description
        );
        let decimals = self
            .rpc
            .call_latest(feed.aggregator, AggregatorV3::decimalsCall {})
            .await?;
        ensure!(decimals <= 77, "unsupported chainlink decimals (>77)");
        let meta = FeedMeta { decimals };
        self.meta.write().await.insert(feed.key(), meta);
        Ok(meta)
    }

    async fn read_feed(&self, feed: &FeedConfig, now: u64) -> Result<FixedValue> {
        let meta = self.feed_meta(feed).await?;
        let round = self
            .rpc
            .call_latest(feed.aggregator, AggregatorV3::latestRoundDataCall {})
            .await?;
        ensure!(
            round.answer.is_positive(),
            "chainlink answer is not positive"
        );
        ensure!(
            round.answeredInRound >= round.roundId,
            "chainlink round is not yet answered"
        );
        let updated = round.updatedAt;
        ensure!(
            updated <= U256::from(now.saturating_add(60)),
            "chainlink round updatedAt is in the future"
        );
        let age = U256::from(now).saturating_sub(updated);
        ensure!(
            age <= U256::from(feed.max_age_secs),
            "chainlink round is stale ({age} s > {} s)",
            feed.max_age_secs
        );
        price_fp18(round.answer.into_raw(), meta.decimals)
    }
}

fn price_fp18(answer: U256, decimals: u8) -> Result<FixedValue> {
    let scaled = U1024::from(answer) * U1024::from(10u64).pow(U1024::from(18u32))
        / U1024::from(10u64).pow(U1024::from(u32::from(decimals)));
    ensure!(
        scaled <= U1024::from(U256::MAX),
        "chainlink price outside FP18 range"
    );
    // Range check above proves the narrowing preserves the value.
    Ok(FixedValue::from_raw(scaled.wrapping_to::<U256>()))
}

#[async_trait]
impl Provider for ChainlinkProvider {
    fn name(&self) -> &str {
        "chainlink"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut tickers = HashMap::new();
        let requested = pairs
            .iter()
            .filter_map(|(base, quote)| self.feeds.get(&format!("{base}/{quote}")))
            .collect::<Vec<_>>();
        if requested.is_empty() {
            return Ok(tickers);
        }
        ensure!(
            self.rpc.chain_id().await? == self.chain_id,
            "chainlink RPC chain ID mismatch"
        );
        // Round age is judged against chain time, not the feeder's wall clock.
        let now = self.rpc.block("latest").await?.timestamp;
        for feed in requested {
            let key = feed.key();
            match self.read_feed(feed, now).await {
                Ok(price) => {
                    if let Some(ticker) =
                        checked_ticker("chainlink", &key, Some(price), VolumeInput::Unavailable)
                    {
                        tickers.insert(key, ticker);
                    }
                }
                Err(error) => {
                    tracing::warn!(provider = "chainlink", feed = %key, error = %error, "chainlink feed skipped");
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
    const NOW: u64 = 1_800_000_000;

    #[derive(Default)]
    struct Fixture {
        wrong_chain: AtomicBool,
        wrong_description: AtomicBool,
        negative: AtomicBool,
        unanswered: AtomicBool,
        /// Seconds between the round's updatedAt and chain time.
        age: AtomicI64,
        fail_round: AtomicBool,
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
                "eth_getBlockByNumber" => {
                    assert_eq!(params[0], "latest");
                    json!({"number":"0x10", "hash":alloy_primitives::B256::repeat_byte(1), "timestamp":format!("0x{NOW:x}")})
                }
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
                        let updated = (NOW as i64 - self.age.load(Ordering::Relaxed)) as u64;
                        let (round, answered) = if self.unanswered.load(Ordering::Relaxed) {
                            (10u128, 9u128)
                        } else {
                            (10u128, 10u128)
                        };
                        (
                            alloy_primitives::aliases::U80::from(round),
                            answer,
                            U256::from(updated),
                            U256::from(updated),
                            alloy_primitives::aliases::U80::from(answered),
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

    fn config(endpoint: &str) -> ChainlinkProviderConfig {
        ChainlinkProviderConfig {
            chain_id: 1,
            rpc_endpoint: endpoint.to_owned(),
            feeds: vec![FeedConfig {
                base: "ETH".into(),
                quote: "840".into(),
                aggregator: AGGREGATOR,
                description: "ETH / USD".into(),
                max_age_secs: 3600,
            }],
        }
    }

    async fn fresh_provider() -> (Arc<Fixture>, test_server::Server, ChainlinkProvider) {
        let fixture = Arc::new(Fixture::default());
        let state = Arc::clone(&fixture);
        let server = test_server::start(Arc::new(move |request| state.response(request))).await;
        let provider = ChainlinkProvider::new(&config(&server.endpoint)).unwrap();
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
        for flag in [&fixture.negative, &fixture.unanswered, &fixture.fail_round] {
            flag.store(true, Ordering::Relaxed);
            assert!(provider
                .get_ticker_prices(&eth_usd())
                .await
                .unwrap()
                .is_empty());
            flag.store(false, Ordering::Relaxed);
        }
        fixture.age.store(3601, Ordering::Relaxed);
        assert!(provider
            .get_ticker_prices(&eth_usd())
            .await
            .unwrap()
            .is_empty());
        fixture.age.store(3600, Ordering::Relaxed);
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

        fixture.wrong_chain.store(true, Ordering::Relaxed);
        assert!(provider.get_ticker_prices(&eth_usd()).await.is_err());
        fixture.wrong_chain.store(false, Ordering::Relaxed);

        // Metadata is cached after the first successful read, so a wrong
        // description only matters for a fresh provider.
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
        bad.feeds[0].max_age_secs = 0;
        assert!(bad.validate().is_err());
        let mut bad = config("http://localhost:8545");
        bad.feeds.push(bad.feeds[0].clone());
        assert!(bad.validate().is_err());
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
            "source without chainlink_providers"
        );
        feeder
            .chainlink_providers
            .push(config("http://localhost:8545"));
        feeder.validate().unwrap();
        feeder.chainlink_providers[0].feeds[0].quote = "USD".into();
        assert!(feeder.validate().is_err(), "source without matching feed");
    }

    #[test]
    fn price_scaling_covers_common_decimals() {
        assert_eq!(
            price_fp18(U256::from(250_000_000_000u64), 8).unwrap(),
            FixedValue::parse("2500").unwrap()
        );
        assert_eq!(
            price_fp18(U256::from(999_900_000_000_000_000u64), 18).unwrap(),
            FixedValue::parse("0.9999").unwrap()
        );
        assert!(price_fp18(U256::MAX, 0).is_err());
    }
}
