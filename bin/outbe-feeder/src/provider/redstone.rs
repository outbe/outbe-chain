//! RedStone price provider via the authenticated data-packages gateway.
//!
//! Every feed answer is a set of signed packages, one per RedStone node. The
//! selection follows the RedStone SDK defaults: keep packages from registered
//! signers that share the newest timestamp, require three distinct signers,
//! take the three values closest to the median, and publish their median.
//! Values are USD quoted and carry no volume. Package signatures are not
//! verified here; the gateway is trusted like Pyth Hermes.

use alloy_primitives::{address, Address};
use async_trait::async_trait;
use eyre::{ensure, eyre, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{checked_ticker, Provider, TickerPrice, VolumeInput};
use crate::config::{FeederConfig, RedstoneConfig};
use crate::fixed::{FixedValue, JsonDecimal};

const DEFAULT_GATEWAY_URL: &str = "https://oracle-gateway.a.redstone.finance";
const DATA_SERVICE_ID: &str = "redstone-primary-prod";
/// `uniqueSignersCount` the RedStone SDK recommends for production.
const UNIQUE_SIGNERS: usize = 3;
/// Packages are produced every few seconds; older than this means a stall.
const MAX_AGE_SECS: u64 = 60;
const MAX_FUTURE_SECS: u64 = 30;

/// Signers registered for `redstone-primary-prod` in the RedStone oracle
/// registry (`packages/sdk/src/registry/initial-state.json`, 2026-10-07).
/// A package from any other address is ignored.
const REGISTERED_SIGNERS: [Address; 17] = [
    address!("0x8cdBF4FE234F106eFB28D84Ef466bBaf8616f8D9"),
    address!("0x8BB8F32Df04c8b654987DAaeD53D6B6091e3B774"),
    address!("0xdEB22f54738d54976C4c0fe5ce6d408E40d88499"),
    address!("0x51Ce04Be4b3E32572C4Ec9135221d0691Ba7d202"),
    address!("0xDD682daEC5A90dD295d14DA4b0bec9281017b5bE"),
    address!("0x9c5AE89C4Af6aA32cE58588DBaF90d18a855B6de"),
    address!("0xe5a346d2EEEc95f7b563b6862732b291E7F96C74"),
    address!("0x04d938651ba3E1313c625244Cb01f0930Afa388B"),
    address!("0x82991bBD3b77De53f9D1ceA88F1B8021f27B557F"),
    address!("0xE6540844F0aE3eC499d365De1f594b2A7c2860E7"),
    address!("0xaB4C37FC34E814e61eC09aE9CE8652CE24EfE53f"),
    address!("0xd0C9878c72C4B8B062378713801Ca7b19117269f"),
    address!("0x6dBB798F484Ae044290d4a03cfA74A3AE760ee54"),
    address!("0xf1B20cf4CFac262D462b919Ad048263B32d682aF"),
    address!("0x711F2DC1C8120f5E927De79e4a1Eec2C35579F2a"),
    address!("0x50d5c34354092790c51e516CE3f7cB8a30b79fF1"),
    address!("0x080B4dB52c765Ef7b0E7156E5171F5e5999D06be"),
];

/// RedStone feed ids are plain USD-quoted base symbols.
fn feed_id(base: &str, quote: &str) -> Option<String> {
    matches!(quote, "USD" | "840").then(|| base.to_uppercase())
}

pub struct RedstoneProvider {
    client: reqwest::Client,
    gateway_url: String,
    api_key: String,
}

impl RedstoneProvider {
    pub fn new(config: &RedstoneConfig) -> Result<Self> {
        ensure!(
            !config.api_key.trim().is_empty(),
            "[redstone] api_key must not be empty"
        );
        let gateway_url = if config.gateway.trim().is_empty() {
            DEFAULT_GATEWAY_URL.to_owned()
        } else {
            config.gateway.trim_end_matches('/').to_owned()
        };
        Ok(Self {
            client: reqwest::Client::new(),
            gateway_url,
            api_key: config.api_key.clone(),
        })
    }
}

/// A `redstone` source needs the `[redstone]` section with a key.
pub(crate) fn validate_config(config: &FeederConfig) -> Result<()> {
    let used = config
        .currency_pairs
        .iter()
        .flat_map(|pair| &pair.sources)
        .any(|source| source.provider == "redstone");
    if !used {
        return Ok(());
    }
    let section = config
        .redstone
        .as_ref()
        .ok_or_else(|| eyre!("provider redstone requires a [redstone] section"))?;
    ensure!(
        !section.api_key.trim().is_empty(),
        "[redstone] api_key must not be empty"
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct SignedPackage {
    #[serde(rename = "timestampMilliseconds")]
    timestamp_ms: u64,
    #[serde(rename = "signerAddress")]
    signer: Address,
    #[serde(rename = "dataPoints")]
    data_points: Vec<DataPoint>,
}

#[derive(Debug, Deserialize)]
struct DataPoint {
    #[serde(rename = "dataFeedId")]
    feed_id: String,
    value: JsonDecimal,
}

/// Median of the [`UNIQUE_SIGNERS`] registered values closest to the overall
/// median among the newest packages, or why the feed is unusable.
fn select_price(feed: &str, packages: &[SignedPackage], now: u64) -> Result<FixedValue> {
    let newest = packages
        .iter()
        .map(|p| p.timestamp_ms / 1000)
        .max()
        .ok_or_else(|| eyre!("no packages"))?;
    if newest > now.saturating_add(MAX_FUTURE_SECS) {
        return Err(eyre!("package timestamp is in the future"));
    }
    if now.saturating_sub(newest) > MAX_AGE_SECS {
        return Err(eyre!("packages are stale ({} s)", now - newest));
    }
    let mut signers = BTreeSet::new();
    let mut values = Vec::new();
    for package in packages.iter().filter(|p| p.timestamp_ms / 1000 == newest) {
        if !REGISTERED_SIGNERS.contains(&package.signer) || !signers.insert(package.signer) {
            continue;
        }
        let value = package
            .data_points
            .iter()
            .find(|point| point.feed_id == feed)
            .and_then(|point| point.value.fixed())
            .filter(|value| !value.is_zero())
            .ok_or_else(|| eyre!("package from {} has no valid value", package.signer))?;
        values.push(value);
    }
    if values.len() < UNIQUE_SIGNERS {
        return Err(eyre!(
            "{} registered signers, need {UNIQUE_SIGNERS}",
            values.len()
        ));
    }
    values.sort_unstable();
    // Lower central value: deterministic and never outside the observed range.
    let median = values[(values.len() - 1) / 2];
    // Keep the UNIQUE_SIGNERS values closest to the median, as the SDK does.
    values.sort_by_key(|value| value.raw().abs_diff(median.raw()));
    let mut closest: Vec<FixedValue> = values.into_iter().take(UNIQUE_SIGNERS).collect();
    closest.sort_unstable();
    Ok(closest[(closest.len() - 1) / 2])
}

#[async_trait]
impl Provider for RedstoneProvider {
    fn name(&self) -> &str {
        "redstone"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();
        let requested: Vec<(String, String)> = pairs
            .iter()
            .filter_map(|(base, quote)| {
                feed_id(base, quote).map(|id| (id, format!("{base}/{quote}")))
            })
            .collect();
        if requested.is_empty() {
            return Ok(result);
        }
        let mut ids: Vec<&str> = requested.iter().map(|(id, _)| id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();

        let url = format!(
            "{}/v2/data-packages/latest-by-data-feeds/{DATA_SERVICE_ID}",
            self.gateway_url
        );
        let response = self
            .client
            .get(&url)
            .query(&[("dataFeedIds", ids.join(","))])
            .header("x-api-key", &self.api_key)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| "redstone gateway request failed")?;
        if !response.status().is_success() {
            return Err(eyre!(
                "redstone gateway returned status {}",
                response.status()
            ));
        }
        let packages: HashMap<String, Vec<SignedPackage>> = response
            .json()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| "failed to decode redstone response")?;

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        for (feed, key) in requested {
            let selected = packages
                .get(&feed)
                .ok_or_else(|| eyre!("feed missing from response"))
                .and_then(|list| select_price(&feed, list, now));
            match selected {
                Ok(price) => {
                    if let Some(ticker) =
                        checked_ticker("redstone", &key, Some(price), VolumeInput::Unavailable)
                    {
                        result.insert(key, ticker);
                    }
                }
                Err(error) => {
                    tracing::warn!(provider = "redstone", feed = %feed, error = %error, "redstone feed skipped");
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn packages(spec: &[(Address, u64, &str)]) -> Vec<SignedPackage> {
        spec.iter()
            .map(|(signer, ts, value)| {
                serde_json::from_value(serde_json::json!({
                    "timestampMilliseconds": ts * 1000,
                    "signerAddress": signer,
                    "dataPoints": [{"dataFeedId": "USDC", "value": serde_json::from_str::<serde_json::Value>(value).unwrap()}],
                }))
                .unwrap()
            })
            .collect()
    }

    fn signer(index: usize) -> Address {
        REGISTERED_SIGNERS[index]
    }

    #[test]
    fn median_of_the_closest_registered_values_at_the_newest_timestamp() {
        let list = packages(&[
            (signer(0), NOW, "0.9999"),
            (signer(1), NOW, "1.0001"),
            (signer(2), NOW, "0.9998"),
            (signer(3), NOW, "1.2"),    // outlier beyond the three closest
            (signer(4), NOW - 10, "5"), // older timestamp
            (signer(0), NOW, "7"),      // duplicate signer
            (Address::repeat_byte(0xaa), NOW, "9"), // not registered
        ]);
        assert_eq!(
            select_price("USDC", &list, NOW).unwrap(),
            FixedValue::parse("0.9999").unwrap()
        );
    }

    #[test]
    fn insufficient_signers_stale_future_and_invalid_values_are_rejected() {
        let two = packages(&[(signer(0), NOW, "1"), (signer(1), NOW, "1")]);
        assert!(select_price("USDC", &two, NOW).is_err());

        let fresh = packages(&[
            (signer(0), NOW, "1"),
            (signer(1), NOW, "1"),
            (signer(2), NOW, "1"),
        ]);
        assert!(select_price("USDC", &fresh, NOW + MAX_AGE_SECS).is_ok());
        assert!(select_price("USDC", &fresh, NOW + MAX_AGE_SECS + 1).is_err());
        assert!(select_price("USDC", &fresh, NOW - MAX_FUTURE_SECS - 1).is_err());

        let zero = packages(&[
            (signer(0), NOW, "1"),
            (signer(1), NOW, "0"),
            (signer(2), NOW, "1"),
        ]);
        assert!(select_price("USDC", &zero, NOW).is_err());
        assert!(
            select_price("USDT", &fresh, NOW).is_err(),
            "feed id mismatch"
        );
        assert!(select_price("USDC", &[], NOW).is_err());

        let unregistered = packages(&[
            (Address::repeat_byte(1), NOW, "1"),
            (Address::repeat_byte(2), NOW, "1"),
            (Address::repeat_byte(3), NOW, "1"),
        ]);
        assert!(select_price("USDC", &unregistered, NOW).is_err());
    }

    #[test]
    fn only_usd_quotes_map_to_feed_ids() {
        assert_eq!(feed_id("usdc", "840").as_deref(), Some("USDC"));
        assert_eq!(feed_id("ETH", "USD").as_deref(), Some("ETH"));
        assert_eq!(feed_id("COEN", "USDT"), None);
    }

    /// Live gateway read; needs a key:
    /// `REDSTONE_API_KEY=... cargo test -p outbe-feeder live_gateway -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_gateway_feeds() {
        let api_key = std::env::var("REDSTONE_API_KEY").expect("REDSTONE_API_KEY");
        let provider = RedstoneProvider::new(&RedstoneConfig {
            api_key,
            gateway: String::new(),
        })
        .unwrap();
        let pairs = vec![("USDC".into(), "840".into()), ("USDT".into(), "840".into())];
        let tickers = provider.get_ticker_prices(&pairs).await.unwrap();
        for key in ["USDC/840", "USDT/840"] {
            eprintln!("{key}: {} FP18", tickers[key].price.raw());
        }
        assert_eq!(tickers.len(), 2);
    }
}
