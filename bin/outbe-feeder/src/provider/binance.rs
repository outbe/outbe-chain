//! Binance price provider.

use async_trait::async_trait;
use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use super::{CandlePrice, Provider, TickerPrice};

/// Maps a (base, quote) pair to a Binance symbol.
/// Returns `None` for pairs Binance doesn't support (e.g. custom tokens).
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    // Skip custom/internal tokens
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = match quote {
        "0xUSD" => "USDT",
        other => other,
    };
    Some(format!("{base}{q}"))
}

/// Binance price provider.
pub struct BinanceProvider {
    client: reqwest::Client,
}

impl BinanceProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct BinanceTickerResponse {
    #[serde(rename = "lastPrice")]
    last_price: String,
    volume: String,
}

#[async_trait]
impl Provider for BinanceProvider {
    fn name(&self) -> &str {
        "binance"
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

            let url = format!("https://api.binance.com/api/v3/ticker/24hr?symbol={symbol}");

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
                        "binance request failed"
                    );
                    continue;
                }
            };

            if !resp.status().is_success() {
                tracing::warn!(
                    status = %resp.status(),
                    pair = %format!("{base}/{quote}"),
                    "binance API error"
                );
                continue;
            }

            let data: BinanceTickerResponse = match resp
                .json()
                .await
                .with_context(|| format!("failed to parse binance response for {base}/{quote}"))
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, pair = %format!("{base}/{quote}"), "binance parse error");
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

    async fn get_candle_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, Vec<CandlePrice>>> {
        let mut result = HashMap::new();

        for (base, quote) in pairs {
            let symbol = match map_symbol(base, quote) {
                Some(s) => s,
                None => continue,
            };

            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol={symbol}&interval=1m&limit=5"
            );

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
                        "binance candles request failed"
                    );
                    continue;
                }
            };

            if !resp.status().is_success() {
                tracing::warn!(
                    status = %resp.status(),
                    pair = %format!("{base}/{quote}"),
                    "binance candles API error"
                );
                continue;
            }

            // Binance klines: [[openTime, open, high, low, close, volume, ...], ...]
            let data: Vec<Vec<serde_json::Value>> = match resp
                .json()
                .await
                .with_context(|| format!("failed to parse binance candles for {base}/{quote}"))
            {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, pair = %format!("{base}/{quote}"), "binance candles parse error");
                    continue;
                }
            };

            let mut candles = Vec::new();
            for kline in &data {
                if kline.len() < 6 {
                    continue;
                }
                let timestamp = kline[0].as_u64().unwrap_or(0) / 1000; // ms -> s
                let close: f64 = kline[4]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                let volume: f64 = kline[5]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);

                if close > 0.0 {
                    candles.push(CandlePrice {
                        price: close,
                        volume,
                        timestamp,
                    });
                }
            }

            if !candles.is_empty() {
                let key = format!("{base}/{quote}");
                result.insert(key, candles);
            }
        }

        Ok(result)
    }
}
