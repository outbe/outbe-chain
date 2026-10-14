//! Kraken price provider.

use async_trait::async_trait;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;

use super::rest::fetch_json;
use crate::fixed::FixedValue;
use crate::provider::{checked_ticker, Provider, TickerPrice, VolumeInput};

/// Maps a (base, quote) pair to a Kraken pair string.
/// Returns `None` for pairs Kraken doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let b = match base {
        "BTC" => "XBT",
        other => other,
    };
    let q = quote;
    Some(format!("{b}{q}"))
}

/// Kraken price provider.
pub struct KrakenProvider {
    client: reqwest::Client,
}

impl KrakenProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct KrakenResponse {
    error: Vec<String>,
    result: Option<HashMap<String, KrakenTickerData>>,
}

#[derive(Debug, Deserialize)]
struct KrakenTickerData {
    /// Last trade: [price, lot volume]
    c: Vec<String>,
    /// Volume: [today, last 24h]
    v: Vec<String>,
}

#[async_trait]
impl Provider for KrakenProvider {
    fn name(&self) -> &str {
        "kraken"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();
        for (base, quote) in pairs {
            let Some(symbol) = map_symbol(base, quote) else {
                continue;
            };
            let key = format!("{base}/{quote}");
            let url = format!("https://api.kraken.com/0/public/Ticker?pair={symbol}");
            let Some(data) = fetch_json::<KrakenResponse>(&self.client, "kraken", &key, &url).await
            else {
                continue;
            };
            if !data.error.is_empty() {
                tracing::warn!(errors = ?data.error, pair = %key, "kraken returned errors");
                continue;
            }
            // Kraken answers under its internal pair name (XETHZUSD), the one
            // entry of the map.
            let Some(ticker) = data.result.as_ref().and_then(|map| map.values().next()) else {
                continue;
            };
            if let Some(ticker) = checked_ticker(
                "kraken",
                &key,
                ticker.c.first().and_then(|value| FixedValue::parse(value)),
                VolumeInput::Present(ticker.v.get(1).and_then(|value| FixedValue::parse(value))),
            ) {
                result.insert(key, ticker);
            }
        }
        Ok(result)
    }
}
