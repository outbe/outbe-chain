//! Huobi (HTX) price provider.

use async_trait::async_trait;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;

use super::rest::fetch_json;
use super::{checked_ticker, Provider, TickerPrice, VolumeInput};
use crate::fixed::JsonDecimal;

/// Maps a (base, quote) pair to a Huobi symbol (lowercase).
/// Returns `None` for pairs Huobi doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = quote;
    Some(format!("{}{}", base.to_lowercase(), q.to_lowercase()))
}

/// Huobi (HTX) price provider.
pub struct HuobiProvider {
    client: reqwest::Client,
}

impl HuobiProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct HuobiResponse {
    status: String,
    tick: Option<HuobiTick>,
}

#[derive(Debug, Deserialize)]
struct HuobiTick {
    close: JsonDecimal,
    vol: JsonDecimal,
}

#[async_trait]
impl Provider for HuobiProvider {
    fn name(&self) -> &str {
        "huobi"
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
            let url = format!("https://api.huobi.pro/market/detail/merged?symbol={symbol}");
            let Some(data) = fetch_json::<HuobiResponse>(&self.client, "huobi", &key, &url).await
            else {
                continue;
            };
            if data.status != "ok" {
                tracing::warn!(status = %data.status, pair = %key, "huobi returned non-ok status");
                continue;
            }
            let Some(tick) = data.tick else {
                continue;
            };
            if let Some(ticker) = checked_ticker(
                "huobi",
                &key,
                tick.close.fixed(),
                VolumeInput::Present(tick.vol.fixed()),
            ) {
                result.insert(key, ticker);
            }
        }
        Ok(result)
    }
}
