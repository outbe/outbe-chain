//! Gate.io price provider.

use async_trait::async_trait;
use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use super::{Provider, TickerPrice};

/// Maps a (base, quote) pair to a Gate.io currency pair.
/// Returns `None` for pairs Gate.io doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = match quote {
        "0xUSD" => "USDT",
        other => other,
    };
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
            let currency_pair = match map_symbol(base, quote) {
                Some(s) => s,
                None => continue,
            };

            let url =
                format!("https://api.gateio.ws/api/v4/spot/tickers?currency_pair={currency_pair}");

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
                        "gate request failed"
                    );
                    continue;
                }
            };

            if !resp.status().is_success() {
                tracing::warn!(
                    status = %resp.status(),
                    pair = %format!("{base}/{quote}"),
                    "gate API error"
                );
                continue;
            }

            let data: Vec<GateTickerResponse> = match resp
                .json()
                .await
                .with_context(|| format!("failed to parse gate response for {base}/{quote}"))
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, pair = %format!("{base}/{quote}"), "gate parse error");
                    continue;
                }
            };

            if let Some(ticker) = data.first() {
                let price: f64 = ticker.last.parse().unwrap_or(0.0);
                let volume: f64 = ticker.base_volume.parse().unwrap_or(0.0);

                if price > 0.0 {
                    let key = format!("{base}/{quote}");
                    result.insert(key, TickerPrice { price, volume });
                }
            }
        }

        Ok(result)
    }
}
