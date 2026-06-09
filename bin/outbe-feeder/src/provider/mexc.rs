//! MEXC price provider.

use async_trait::async_trait;
use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use super::{Provider, TickerPrice};

/// Maps a (base, quote) pair to a MEXC symbol.
/// Returns `None` for pairs MEXC doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = match quote {
        "0xUSD" => "USDT",
        other => other,
    };
    Some(format!("{base}{q}"))
}

/// MEXC price provider.
pub struct MexcProvider {
    client: reqwest::Client,
}

impl MexcProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct MexcTickerResponse {
    #[serde(rename = "lastPrice")]
    last_price: String,
    volume: String,
}

#[async_trait]
impl Provider for MexcProvider {
    fn name(&self) -> &str {
        "mexc"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();

        for (base, quote) in pairs {
            let symbol = match map_symbol(base, quote) {
                Some(s) => s,
                None => continue,
            };

            let url = format!("https://api.mexc.com/api/v3/ticker/24hr?symbol={symbol}");

            let resp = match self
                .client
                .get(&url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        pair = %format!("{base}/{quote}"),
                        "mexc request failed"
                    );
                    continue;
                }
            };

            if !resp.status().is_success() {
                tracing::warn!(
                    status = %resp.status(),
                    pair = %format!("{base}/{quote}"),
                    "mexc API error"
                );
                continue;
            }

            let data: MexcTickerResponse = match resp
                .json()
                .await
                .with_context(|| format!("failed to parse mexc response for {base}/{quote}"))
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, pair = %format!("{base}/{quote}"), "mexc parse error");
                    continue;
                }
            };

            let price: f64 = data.last_price.parse().unwrap_or(0.0);
            let volume: f64 = data.volume.parse().unwrap_or(0.0);

            if price > 0.0 {
                let key = format!("{base}/{quote}");
                result.insert(key, TickerPrice { price, volume });
            }
        }

        Ok(result)
    }
}
