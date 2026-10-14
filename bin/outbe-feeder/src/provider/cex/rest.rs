//! One HTTP read shared by the REST exchange providers.

use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

use crate::fixed::FixedValue;
use crate::provider::{checked_ticker, TickerPrice, VolumeInput};

/// GET `url` and decode its JSON body. A failed request, a non-2xx status or
/// an undecodable body is logged for `provider` and `pair` and yields `None`,
/// so one bad market never fails the whole vote.
pub(crate) async fn fetch_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    provider: &'static str,
    pair: &str,
    url: &str,
) -> Option<T> {
    let response = match client.get(url).timeout(Duration::from_secs(5)).send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(provider, pair, error = %error, "request failed");
            return None;
        }
    };
    if !response.status().is_success() {
        tracing::warn!(provider, pair, status = %response.status(), "API error");
        return None;
    }
    match response.json().await {
        Ok(data) => Some(data),
        Err(error) => {
            tracing::warn!(provider, pair, error = %error, "parse error");
            None
        }
    }
}

/// The `/api/v3/ticker/24hr` body Binance and MEXC share.
#[derive(Debug, Deserialize)]
pub(crate) struct Ticker24h {
    #[serde(rename = "lastPrice")]
    last_price: String,
    volume: String,
}

/// Reads the 24h ticker of every pair `symbol_of` maps from `endpoint`, the
/// `/api/v3/ticker/24hr` URL Binance and MEXC both serve.
pub(crate) async fn ticker_24h(
    client: &reqwest::Client,
    provider: &'static str,
    endpoint: &str,
    pairs: &[(String, String)],
    symbol_of: fn(&str, &str) -> Option<String>,
) -> HashMap<String, TickerPrice> {
    let mut result = HashMap::new();
    for (base, quote) in pairs {
        let Some(symbol) = symbol_of(base, quote) else {
            continue;
        };
        let key = format!("{base}/{quote}");
        let url = format!("{endpoint}?symbol={symbol}");
        let Some(data) = fetch_json::<Ticker24h>(client, provider, &key, &url).await else {
            continue;
        };
        if let Some(ticker) = checked_ticker(
            provider,
            &key,
            FixedValue::parse(&data.last_price),
            VolumeInput::Present(FixedValue::parse(&data.volume)),
        ) {
            result.insert(key, ticker);
        }
    }
    result
}
