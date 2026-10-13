//! Binance price provider.

use async_trait::async_trait;
use eyre::Result;
use std::collections::HashMap;

use super::rest::{fetch_json, ticker_24h};
use crate::fixed::FixedValue;
use crate::provider::{checked_candle, CandlePrice, Provider, TickerPrice, VolumeInput};

/// Maps a (base, quote) pair to a Binance symbol.
/// Returns `None` for pairs Binance doesn't support (e.g. custom tokens).
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    // Skip custom/internal tokens
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = quote;
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

#[async_trait]
impl Provider for BinanceProvider {
    fn name(&self) -> &str {
        "binance"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        Ok(ticker_24h(
            &self.client,
            "binance",
            "https://api.binance.com/api/v3/ticker/24hr",
            pairs,
            map_symbol,
        )
        .await)
    }

    async fn get_candle_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, Vec<CandlePrice>>> {
        let mut result = HashMap::new();
        for (base, quote) in pairs {
            let Some(symbol) = map_symbol(base, quote) else {
                continue;
            };
            let key = format!("{base}/{quote}");
            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol={symbol}&interval=1m&limit=5"
            );
            // Binance klines: [[openTime, open, high, low, close, volume, ...], ...]
            let Some(klines) =
                fetch_json::<Vec<Vec<serde_json::Value>>>(&self.client, "binance", &key, &url)
                    .await
            else {
                continue;
            };
            let candles: Vec<CandlePrice> = klines
                .iter()
                .filter(|kline| kline.len() >= 6)
                .filter_map(|kline| {
                    checked_candle(
                        "binance",
                        &key,
                        kline[4].as_str().and_then(FixedValue::parse),
                        VolumeInput::Present(kline[5].as_str().and_then(FixedValue::parse)),
                        kline[0].as_u64().unwrap_or(0) / 1000, // ms -> s
                    )
                })
                .collect();
            if !candles.is_empty() {
                result.insert(key, candles);
            }
        }
        Ok(result)
    }
}
