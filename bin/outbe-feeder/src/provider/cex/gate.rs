//! Gate.io price provider.

use async_trait::async_trait;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;

use super::rest::fetch_json;
use crate::fixed::FixedValue;
use crate::provider::{checked_ticker, Provider, TickerPrice, VolumeInput};

/// Maps a (base, quote) pair to a Gate.io currency pair.
/// Returns `None` for pairs Gate.io doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = quote;
    Some(format!("{base}_{q}"))
}

/// Gate.io price provider.
pub struct GateProvider {
    client: reqwest::Client,
}

impl GateProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GateTickerResponse {
    last: String,
    base_volume: String,
}

#[async_trait]
impl Provider for GateProvider {
    fn name(&self) -> &str {
        "gate"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();
        for (base, quote) in pairs {
            let Some(currency_pair) = map_symbol(base, quote) else {
                continue;
            };
            let key = format!("{base}/{quote}");
            let url =
                format!("https://api.gateio.ws/api/v4/spot/tickers?currency_pair={currency_pair}");
            let Some(data) =
                fetch_json::<Vec<GateTickerResponse>>(&self.client, "gate", &key, &url).await
            else {
                continue;
            };
            let Some(ticker) = data.first() else {
                continue;
            };
            if let Some(ticker) = checked_ticker(
                "gate",
                &key,
                FixedValue::parse(&ticker.last),
                VolumeInput::Present(FixedValue::parse(&ticker.base_volume)),
            ) {
                result.insert(key, ticker);
            }
        }
        Ok(result)
    }
}
