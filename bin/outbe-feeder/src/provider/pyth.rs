//! Pyth Network price provider via the Hermes REST API.

use async_trait::async_trait;
use eyre::{eyre, Context, Result};
use outbe_primitives::stablecoin::iso_4217_alpha;
use outbe_primitives::units::SCALE_1E18;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;

use super::onchain_feed::MAX_FEED_AGE_SECS;
use super::{checked_ticker, Provider, TickerPrice, VolumeInput};
use crate::config::ApiProviderConfig;
use crate::fixed::FixedValue;

/// Hermes after the Pyth Core upgrade (2026-08-26). It requires a key.
const DEFAULT_HERMES_URL: &str = "https://pyth.dourolabs.app/hermes";

/// Pyth Network price provider.
pub struct PythProvider {
    client: reqwest::Client,
    hermes_url: String,
    api_key: String,
    /// Feed symbol (`FX.EUR/USD`) to feed id, loaded once.
    feeds: OnceCell<HashMap<String, String>>,
}

impl PythProvider {
    pub fn new(endpoint: Option<&ApiProviderConfig>) -> Result<Self> {
        let api_key = endpoint.map_or("", |e| e.api_key.trim()).to_owned();
        eyre::ensure!(!api_key.is_empty(), "api provider pyth needs an api_key");
        let rest = endpoint.map_or("", |e| e.rest.trim());
        let hermes_url = if rest.is_empty() {
            DEFAULT_HERMES_URL.to_owned()
        } else {
            rest.trim_end_matches('/').to_owned()
        };
        Ok(Self {
            client: reqwest::Client::new(),
            hermes_url,
            api_key,
            feeds: OnceCell::new(),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .client
            .get(format!("{}{path}", self.hermes_url))
            .bearer_auth(&self.api_key)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| "pyth request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let path = path.split('?').next().unwrap_or(path);
            return Err(eyre!(
                "pyth {path} returned status {status}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        response
            .json()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| "failed to decode pyth response")
    }

    async fn feeds(&self) -> Result<&HashMap<String, String>> {
        self.feeds
            .get_or_try_init(|| async {
                let list: Vec<FeedInfo> = self.get("/v2/price_feeds").await?;
                Ok(list
                    .into_iter()
                    .map(|feed| (feed.attributes.symbol, feed.id))
                    .collect())
            })
            .await
    }
}

/// Pyth symbols that price `base` in dollars, each with whether Pyth quotes
/// it the other way round. An explicit symbol (`FX.USD/JPY`) is used as
/// given. An ISO numeric base tries `FX.<ALPHA>/USD`, then `FX.USD/<ALPHA>`.
/// Any other base reads `Crypto.<BASE>/USD`.
fn candidates(base: &str, quote: &str) -> Vec<(String, bool)> {
    if !matches!(quote, "USD" | "840") {
        return Vec::new();
    }
    if let Some((_, pair)) = base.split_once('.') {
        return vec![(base.to_owned(), pair.starts_with("USD/"))];
    }
    let alpha = base
        .parse::<u16>()
        .ok()
        .and_then(iso_4217_alpha)
        .map(|code| String::from_utf8_lossy(&code).into_owned());
    match alpha {
        Some(alpha) => vec![
            (format!("FX.{alpha}/USD"), false),
            (format!("FX.USD/{alpha}"), true),
        ],
        None => vec![(format!("Crypto.{}/USD", base.to_uppercase()), false)],
    }
}

/// `1 / price` at the FP18 scale.
fn inverse(price: FixedValue) -> Option<FixedValue> {
    (!price.is_zero()).then(|| FixedValue::from_raw(SCALE_1E18 * SCALE_1E18 / price.raw()))
}

#[derive(Debug, Deserialize)]
struct FeedInfo {
    id: String,
    attributes: FeedAttributes,
}

#[derive(Debug, Deserialize)]
struct FeedAttributes {
    #[serde(default)]
    symbol: String,
}

#[derive(Debug, Deserialize)]
struct PythPriceResponse {
    parsed: Vec<PythParsedPrice>,
}

#[derive(Debug, Deserialize)]
struct PythParsedPrice {
    id: String,
    price: PythPriceData,
}

#[derive(Debug, Deserialize)]
struct PythPriceData {
    price: String,
    expo: i32,
    publish_time: u64,
}

#[async_trait]
impl Provider for PythProvider {
    fn name(&self) -> &str {
        "pyth"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let feeds = self.feeds().await?;
        // Feed id -> (market key, inverted).
        let mut wanted: HashMap<&str, Vec<(String, bool)>> = HashMap::new();
        for (base, quote) in pairs {
            let key = format!("{base}/{quote}");
            let found = candidates(base, quote)
                .into_iter()
                .find_map(|(symbol, inverted)| feeds.get(&symbol).map(|id| (id, inverted)));
            match found {
                Some((id, inverted)) => wanted.entry(id).or_default().push((key, inverted)),
                None => tracing::warn!(market = %key, "pyth has no feed for market"),
            }
        }
        let mut result = HashMap::new();
        if wanted.is_empty() {
            return Ok(result);
        }
        // One request per feed: a feed the key is not entitled to (403) drops
        // alone instead of failing every market.
        let responses = futures::future::join_all(wanted.keys().map(|id| async move {
            let response = self
                .get::<PythPriceResponse>(&format!(
                    "/v2/updates/price/latest?ids[]={id}&parsed=true"
                ))
                .await;
            if let Err(error) = &response {
                tracing::warn!(feed = %id, %error, "pyth feed unavailable");
            }
            response
        }))
        .await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        for parsed in responses.into_iter().flatten().flat_map(|data| data.parsed) {
            let Some(markets) = wanted.get(parsed.id.as_str()) else {
                continue;
            };
            let age = now.saturating_sub(parsed.price.publish_time);
            if age > MAX_FEED_AGE_SECS {
                tracing::warn!(feed = %parsed.id, age, "pyth price is stale");
                continue;
            }
            let price = FixedValue::parse(&format!("{}e{}", parsed.price.price, parsed.price.expo));
            for (key, inverted) in markets {
                let price = if *inverted {
                    price.and_then(inverse)
                } else {
                    price
                };
                if let Some(ticker) = checked_ticker("pyth", key, price, VolumeInput::Unavailable) {
                    result.insert(key.clone(), ticker);
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_bases_try_both_directions_and_symbols_are_used_as_given() {
        assert_eq!(
            candidates("978", "840"),
            vec![
                ("FX.EUR/USD".to_owned(), false),
                ("FX.USD/EUR".to_owned(), true)
            ]
        );
        assert_eq!(
            candidates("FX.USD/JPY", "USD"),
            vec![("FX.USD/JPY".to_owned(), true)]
        );
        assert_eq!(
            candidates("eth", "840"),
            vec![("Crypto.ETH/USD".to_owned(), false)]
        );
        assert!(candidates("978", "USDT").is_empty());
    }

    #[test]
    fn inverse_turns_dollars_per_unit_around() {
        let usd_jpy = FixedValue::parse("160").unwrap();
        assert_eq!(inverse(usd_jpy), FixedValue::parse("0.00625"));
        assert_eq!(inverse(FixedValue::ZERO), None);
    }

    /// Live Hermes read. It needs a key:
    /// `PYTH_API_KEY=... cargo test -p outbe-feeder live_hermes -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_hermes_fx() {
        let endpoint = ApiProviderConfig {
            name: "pyth".into(),
            rest: String::new(),
            websocket: String::new(),
            api_key: std::env::var("PYTH_API_KEY").expect("PYTH_API_KEY"),
        };
        let provider = PythProvider::new(Some(&endpoint)).unwrap();
        // The trial key covers EUR/USD, USD/JPY and USD/CNH, not GBP or CHF.
        let pairs: Vec<(String, String)> = ["978", "392", "FX.USD/CNH"]
            .into_iter()
            .map(|base| (base.to_owned(), "840".to_owned()))
            .collect();
        let tickers = provider.get_ticker_prices(&pairs).await.unwrap();
        for (base, quote) in &pairs {
            let key = format!("{base}/{quote}");
            eprintln!("{key}: {} FP18", tickers[&key].price.raw());
        }
        assert_eq!(tickers.len(), 3);
    }
}
