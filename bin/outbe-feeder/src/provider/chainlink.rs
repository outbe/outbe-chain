//! Chainlink / CryptoCompare price provider.
//!
//! Uses the CryptoCompare REST API (same data source as Cosmos oracle's
//! Chainlink provider) to fetch ticker and candle data.

use async_trait::async_trait;
use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use super::{CandlePrice, Provider, TickerPrice};

const CRYPTOCOMPARE_BASE_URL: &str = "https://min-api.cryptocompare.com";

/// Maps a trading pair to CryptoCompare symbol format.
/// Returns `(fsym, tsym)` or None if the pair is not supported.
fn map_symbol(base: &str, quote: &str) -> Option<(String, String)> {
    let fsym = match base {
        "COEN" => return None, // Internal token, not on CryptoCompare
        other => other.to_uppercase(),
    };
    let tsym = match quote {
        "0xUSD" => "USD".to_string(),
        other => other.to_uppercase(),
    };
    Some((fsym, tsym))
}

/// Chainlink/CryptoCompare price provider.
pub struct ChainlinkProvider {
    client: reqwest::Client,
}

impl ChainlinkProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct CryptoCompareTickerResponse {
    #[serde(rename = "RAW")]
    raw: Option<HashMap<String, HashMap<String, CryptoCompareRawTicker>>>,
}

#[derive(Debug, Deserialize)]
struct CryptoCompareRawTicker {
    #[serde(rename = "PRICE")]
    price: Option<f64>,
    #[serde(rename = "VOLUME24HOUR")]
    volume_24h: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CryptoCompareCandleResponse {
    #[serde(rename = "Data")]
    data: Option<CryptoCompareCandleData>,
}

#[derive(Debug, Deserialize)]
struct CryptoCompareCandleData {
    #[serde(rename = "Data")]
    data: Option<Vec<CryptoCompareCandle>>,
}

#[derive(Debug, Deserialize)]
struct CryptoCompareCandle {
    time: u64,
    close: f64,
    volumeto: f64,
}

#[async_trait]
impl Provider for ChainlinkProvider {
    fn name(&self) -> &str {
        "chainlink"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut result = HashMap::new();

        // Collect all supported symbols for a batch request
        let mapped: Vec<(String, String, String, String)> = pairs
            .iter()
            .filter_map(|(base, quote)| {
                map_symbol(base, quote)
                    .map(|(fsym, tsym)| (base.clone(), quote.clone(), fsym, tsym))
            })
            .collect();

        if mapped.is_empty() {
            return Ok(result);
        }

        // CryptoCompare supports multi-pair queries
        let fsyms: Vec<&str> = mapped.iter().map(|(_, _, f, _)| f.as_str()).collect();
        let tsyms: Vec<&str> = mapped.iter().map(|(_, _, _, t)| t.as_str()).collect();

        let fsyms_str = fsyms.join(",");
        let tsyms_str = tsyms.join(",");

        let url = format!(
            "{}/data/pricemultifull?fsyms={}&tsyms={}",
            CRYPTOCOMPARE_BASE_URL, fsyms_str, tsyms_str
        );

        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .with_context(|| "cryptocompare ticker request failed")?;

        if !resp.status().is_success() {
            tracing::warn!(
                status = %resp.status(),
                "cryptocompare API error"
            );
            return Ok(result);
        }

        let data: CryptoCompareTickerResponse = resp
            .json()
            .await
            .with_context(|| "failed to parse cryptocompare response")?;

        if let Some(raw) = data.raw {
            for (base, quote, fsym, tsym) in &mapped {
                if let Some(tsym_map) = raw.get(fsym.as_str()) {
                    if let Some(ticker) = tsym_map.get(tsym.as_str()) {
                        if let Some(price) = ticker.price {
                            if price > 0.0 {
                                let key = format!("{base}/{quote}");
                                result.insert(
                                    key,
                                    TickerPrice {
                                        price,
                                        volume: ticker.volume_24h.unwrap_or(0.0),
                                    },
                                );
                            }
                        }
                    }
                }
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
            let (fsym, tsym) = match map_symbol(base, quote) {
                Some(s) => s,
                None => continue,
            };

            let url = format!(
                "{}/data/v2/histominute?fsym={}&tsym={}&limit=5",
                CRYPTOCOMPARE_BASE_URL, fsym, tsym
            );

            let resp = self
                .client
                .get(&url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, pair = %format!("{base}/{quote}"), "cryptocompare candle request failed");
                    continue;
                }
            };

            if !resp.status().is_success() {
                continue;
            }

            let data: CryptoCompareCandleResponse = match resp.json().await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to parse cryptocompare candle response");
                    continue;
                }
            };

            if let Some(outer) = data.data {
                if let Some(candles) = outer.data {
                    let key = format!("{base}/{quote}");
                    let entries: Vec<CandlePrice> = candles
                        .into_iter()
                        .filter(|c| c.close > 0.0)
                        .map(|c| CandlePrice {
                            price: c.close,
                            volume: c.volumeto,
                            timestamp: c.time,
                        })
                        .collect();
                    if !entries.is_empty() {
                        result.insert(key, entries);
                    }
                }
            }
        }

        Ok(result)
    }
}
