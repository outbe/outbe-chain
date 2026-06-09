//! Mock HTTP price provider.
//!
//! Ports the Cosmos feeder `mock_http` provider shape. It reads prices from a
//! configurable REST endpoint:
//! - `GET /api/tickers?symbols=COEN0XUSD,ETHUSDC`
//! - `GET /api/candles?symbols=COEN0XUSD,ETHUSDC`
//!
//! Responses are expected to use `{ "data": [...] }` with string or numeric
//! price/volume fields.

use async_trait::async_trait;
use eyre::{eyre, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use crate::config::ProviderEndpointConfig;

use super::{CandlePrice, Provider, TickerPrice};

const DEFAULT_MOCK_HTTP_URL: &str = "http://localhost:8000";

#[derive(Debug)]
pub struct MockHttpProvider {
    base_url: String,
    client: reqwest::Client,
}

impl MockHttpProvider {
    pub fn new(endpoint: &ProviderEndpointConfig) -> Result<Self> {
        let base_url = if endpoint.rest.trim().is_empty() {
            DEFAULT_MOCK_HTTP_URL.to_string()
        } else {
            endpoint.rest.trim_end_matches('/').to_string()
        };

        Ok(Self {
            base_url,
            client: reqwest::Client::new(),
        })
    }

    fn url_for(&self, path: &str, pairs: &[(String, String)]) -> String {
        let symbols = pairs
            .iter()
            .map(|(base, quote)| pair_symbol(base, quote))
            .collect::<Vec<_>>()
            .join(",");
        format!("{}/api/{}?symbols={}", self.base_url, path, symbols)
    }
}

#[derive(Debug, Deserialize)]
struct TickerResponse {
    #[serde(default)]
    data: Vec<TickerEntry>,
}

#[derive(Debug, Deserialize)]
struct TickerEntry {
    symbol: String,
    price: NumberLike,
    volume: NumberLike,
}

#[derive(Debug, Deserialize)]
struct CandleResponse {
    #[serde(default)]
    data: Vec<CandleEntry>,
}

#[derive(Debug, Deserialize)]
struct CandleEntry {
    symbol: String,
    price: NumberLike,
    volume: NumberLike,
    #[serde(default)]
    timestamp: i64,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NumberLike {
    String(String),
    Number(f64),
}

impl NumberLike {
    fn as_f64(&self) -> Result<f64> {
        match self {
            Self::String(value) => value
                .parse::<f64>()
                .with_context(|| format!("failed to parse numeric string `{value}`")),
            Self::Number(value) => Ok(*value),
        }
    }
}

#[async_trait]
impl Provider for MockHttpProvider {
    fn name(&self) -> &str {
        "mock_http"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let requested = requested_symbols(pairs);
        let url = self.url_for("tickers", pairs);
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("failed to fetch mock_http tickers from {url}"))?;

        if !response.status().is_success() {
            return Err(eyre!(
                "mock_http ticker endpoint returned status {}",
                response.status()
            ));
        }

        let body: TickerResponse = response
            .json()
            .await
            .with_context(|| "failed to decode mock_http ticker response")?;

        tracing::debug!(url = %url, entries = body.data.len(), "mock_http ticker response");

        let mut prices = HashMap::new();
        for ticker in body.data {
            let symbol = ticker.symbol.to_uppercase();
            let Some(key) = requested.get(&symbol) else {
                continue;
            };
            if prices.contains_key(key) {
                return Err(eyre!("duplicate mock_http ticker for {symbol}"));
            }

            let price = ticker.price.as_f64()?;
            let volume = ticker.volume.as_f64()?;
            tracing::info!(symbol = %symbol, price, volume, "mock_http ticker received");
            if price > 0.0 {
                prices.insert(key.clone(), TickerPrice { price, volume });
            }
        }

        for (symbol, key) in &requested {
            if !prices.contains_key(key) {
                return Err(eyre!("missing mock_http ticker for {symbol}"));
            }
        }

        Ok(prices)
    }

    async fn get_candle_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, Vec<CandlePrice>>> {
        let requested = requested_symbols(pairs);
        let url = self.url_for("candles", pairs);
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("failed to fetch mock_http candles from {url}"))?;

        if !response.status().is_success() {
            return Err(eyre!(
                "mock_http candle endpoint returned status {}",
                response.status()
            ));
        }

        let body: CandleResponse = response
            .json()
            .await
            .with_context(|| "failed to decode mock_http candle response")?;

        tracing::debug!(url = %url, entries = body.data.len(), "mock_http candle response");

        let mut candles: HashMap<String, Vec<CandlePrice>> = HashMap::new();
        for candle in body.data {
            let symbol = candle.symbol.to_uppercase();
            let Some(key) = requested.get(&symbol) else {
                continue;
            };

            let price = candle.price.as_f64()?;
            let volume = candle.volume.as_f64()?;
            if price <= 0.0 || volume <= 0.0 {
                continue;
            }

            candles.entry(key.clone()).or_default().push(CandlePrice {
                price,
                volume,
                timestamp: candle.timestamp.max(0) as u64,
            });
        }

        if candles.is_empty() {
            return self.get_ticker_prices(pairs).await.map(|tickers| {
                tickers
                    .into_iter()
                    .map(|(key, ticker)| {
                        (
                            key,
                            vec![CandlePrice {
                                price: ticker.price,
                                volume: ticker.volume,
                                timestamp: 0,
                            }],
                        )
                    })
                    .collect()
            });
        }

        Ok(candles)
    }
}

fn requested_symbols(pairs: &[(String, String)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(base, quote)| (pair_symbol(base, quote), format!("{base}/{quote}")))
        .collect()
}

fn pair_symbol(base: &str, quote: &str) -> String {
    format!("{base}{quote}").to_uppercase()
}
