//! Coinbase price provider.

use async_trait::async_trait;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;

use super::rest::fetch_json;
use crate::fixed::FixedValue;
use crate::provider::{checked_ticker, Provider, TickerPrice, VolumeInput};

/// Maps a (base, quote) pair to a Coinbase price path segment.
/// Returns `None` for pairs Coinbase doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = quote;
    Some(format!("{base}-{q}"))
}

/// Coinbase price provider.
pub struct CoinbaseProvider {
    client: reqwest::Client,
}

impl CoinbaseProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct CoinbaseResponse {
    data: CoinbasePriceData,
}

#[derive(Debug, Deserialize)]
struct CoinbasePriceData {
    amount: String,
}

#[async_trait]
impl Provider for CoinbaseProvider {
    fn name(&self) -> &str {
        "coinbase"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();
        for (base, quote) in pairs {
            let Some(pair_slug) = map_symbol(base, quote) else {
                continue;
            };
            let key = format!("{base}/{quote}");
            let url = format!("https://api.coinbase.com/v2/prices/{pair_slug}/spot");
            let Some(data) =
                fetch_json::<CoinbaseResponse>(&self.client, "coinbase", &key, &url).await
            else {
                continue;
            };
            if let Some(ticker) = checked_ticker(
                "coinbase",
                &key,
                FixedValue::parse(&data.data.amount),
                VolumeInput::Unavailable,
            ) {
                result.insert(key, ticker);
            }
        }
        Ok(result)
    }
}
