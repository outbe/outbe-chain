//! Price provider trait and implementations.

pub mod binance;
pub mod chainlink;
pub mod coinbase;
pub mod gate;
pub mod huobi;
pub mod kraken;
pub mod mexc;
pub mod mock;
pub mod mock_http;
pub mod okx;
pub mod pyth;

use eyre::{eyre, Result};
use std::collections::HashMap;

use crate::config::{FeederConfig, ProviderEndpointConfig};

/// A price data point from a provider.
///
/// Uses `f64` intentionally: provider REST APIs return JSON floats.
/// This is the off-chain ingestion boundary. Values are converted to
/// `U256` at 1e18 scale in the aggregator before building the on-chain
/// vote payload. The on-chain oracle (`crates/system/oracle/`) uses
/// only `U256` — no `f64` crosses the precompile boundary.
#[derive(Debug, Clone)]
pub struct TickerPrice {
    /// Last trade price (off-chain f64 from provider API).
    pub price: f64,
    /// 24-hour trading volume (off-chain f64 from provider API).
    pub volume: f64,
}

/// A single candle (OHLCV bar) from a provider.
///
/// Same `f64` boundary rationale as [`TickerPrice`]. Converted to
/// `U256` fixed-point by the aggregator's TVWAP computation.
#[derive(Debug, Clone)]
pub struct CandlePrice {
    /// Close price of the candle (off-chain f64).
    pub price: f64,
    /// Volume during the candle period (off-chain f64).
    pub volume: f64,
    /// Unix timestamp (seconds) of the candle open.
    /// Currently unused by the aggregator's TVWAP (which treats all candles as
    /// equal-duration), but retained for future time-duration weighting where
    /// each candle's weight is proportional to its actual time span.
    #[allow(dead_code)]
    pub timestamp: u64,
}

/// Trait for external price data providers.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Returns the provider name.
    fn name(&self) -> &str;

    /// Fetches current ticker prices for the given pairs.
    /// Keys are `"BASE/QUOTE"` strings.
    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>>;

    /// Fetches recent candle data for the given pairs.
    /// Keys are `"BASE/QUOTE"` strings; values are chronologically ordered candles.
    /// Default returns empty — providers that don't support candles need not override.
    async fn get_candle_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, Vec<CandlePrice>>> {
        let _ = pairs;
        Ok(HashMap::new())
    }
}

/// Creates provider instances from configuration.
pub fn create_providers(config: &FeederConfig) -> Result<Vec<Box<dyn Provider>>> {
    let mut providers: Vec<Box<dyn Provider>> = Vec::new();

    // Collect unique provider names from all pair configs
    let mut provider_names: Vec<String> = config
        .currency_pairs
        .iter()
        .flat_map(|p| p.providers.clone())
        .collect();
    provider_names.sort();
    provider_names.dedup();

    let endpoints: HashMap<&str, &ProviderEndpointConfig> = config
        .provider_endpoints
        .iter()
        .map(|endpoint| (endpoint.name.as_str(), endpoint))
        .collect();

    for name in &provider_names {
        let provider: Box<dyn Provider> = match name.as_str() {
            "mock" => Box::new(mock::MockProvider::new()),
            "mock_http" => {
                let endpoint = endpoints.get("mock_http").ok_or_else(|| {
                    eyre!("provider mock_http requires a [[provider_endpoints]] entry")
                })?;
                Box::new(mock_http::MockHttpProvider::new(endpoint)?)
            }
            "pyth" => Box::new(pyth::PythProvider::new()?),
            "chainlink" => Box::new(chainlink::ChainlinkProvider::new()?),
            "binance" => Box::new(binance::BinanceProvider::new()?),
            "kraken" => Box::new(kraken::KrakenProvider::new()?),
            "okx" => Box::new(okx::OkxProvider::new()?),
            "gate" => Box::new(gate::GateProvider::new()?),
            "huobi" => Box::new(huobi::HuobiProvider::new()?),
            "mexc" => Box::new(mexc::MexcProvider::new()?),
            "coinbase" => Box::new(coinbase::CoinbaseProvider::new()?),
            other => {
                tracing::warn!(provider = other, "unknown provider, skipping");
                continue;
            }
        };
        providers.push(provider);
    }

    Ok(providers)
}
