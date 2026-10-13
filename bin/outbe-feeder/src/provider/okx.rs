//! OKX price provider.

use async_trait::async_trait;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;

use super::rest::fetch_json;
use super::{checked_ticker, Provider, TickerPrice, VolumeInput};
use crate::fixed::FixedValue;

/// Maps a (base, quote) pair to an OKX instrument ID.
/// Returns `None` for pairs OKX doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = quote;
    Some(format!("{base}-{q}"))
}

/// OKX price provider.
pub struct OkxProvider {
    client: reqwest::Client,
}

impl OkxProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct OkxResponse {
    data: Vec<OkxTickerData>,
}

#[derive(Debug, Deserialize)]
struct OkxTickerData {
    last: String,
    #[serde(rename = "vol24h")]
    vol_24h: String,
}

#[async_trait]
impl Provider for OkxProvider {
    fn name(&self) -> &str {
        "okx"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();
        for (base, quote) in pairs {
            let Some(inst_id) = map_symbol(base, quote) else {
                continue;
            };
            let key = format!("{base}/{quote}");
            let url = format!("https://www.okx.com/api/v5/market/ticker?instId={inst_id}");
            let Some(data) = fetch_json::<OkxResponse>(&self.client, "okx", &key, &url).await
            else {
                continue;
            };
            let Some(ticker) = data.data.first() else {
                continue;
            };
            if let Some(ticker) = checked_ticker(
                "okx",
                &key,
                FixedValue::parse(&ticker.last),
                VolumeInput::Present(FixedValue::parse(&ticker.vol_24h)),
            ) {
                result.insert(key, ticker);
            }
        }
        Ok(result)
    }
}
