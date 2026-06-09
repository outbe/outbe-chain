//! Price aggregation with TVWAP and VWAP computation and deviation filtering.
//!
//! The aggregator uses a two-tier pricing strategy inspired by the Cosmos
//! oracle feeder: **candle TVWAP first, ticker VWAP fallback**.
//!
//! 1. If any provider returns candle data for a pair, compute a
//!    Time-Volume-Weighted Average Price (TVWAP) across all candles.
//! 2. If no candles are available, fall back to the original ticker-based
//!    Volume-Weighted Average Price (VWAP) across providers.
//!
//! All price/volume arithmetic uses U256 fixed-point at 1e18 scale
//! to match on-chain Oracle semantics. f64 is only used for provider
//! ingestion (parsing REST responses) and deviation filtering (σ-based
//! statistical test), never for final VWAP/TVWAP output.

use crate::config::FeederConfig;
use crate::provider::{CandlePrice, Provider, TickerPrice};
use alloy_primitives::U256;
use eyre::Result;
use outbe_primitives::units::SCALE_1E18_U128;
use std::collections::HashMap;

/// Aggregated price/volume for a single pair (U256 fixed-point at 1e18 scale).
#[derive(Debug, Clone)]
pub struct AggregatedPrice {
    pub base: String,
    pub quote: String,
    /// VWAP price at 1e18 scale.
    pub price: U256,
    /// Total volume at 1e18 scale.
    pub volume: U256,
}

/// Converts a positive, finite f64 to U256 at 1e18 scale.
/// Returns ZERO for non-positive, NaN, or infinite values.
fn f64_to_u256(value: f64) -> U256 {
    if value <= 0.0 || !value.is_finite() {
        return U256::ZERO;
    }
    let scaled = (value * SCALE_1E18_U128 as f64) as u128;
    U256::from(scaled)
}

/// Fetches prices from providers, filters outliers, and computes the best
/// available weighted average price.
///
/// Strategy per pair:
/// 1. If any configured provider returns candle data → compute TVWAP.
/// 2. Otherwise fall back to ticker-based VWAP.
///
/// Only providers listed in each pair's `providers` config are consulted.
pub async fn fetch_and_aggregate(
    providers: &[Box<dyn Provider>],
    config: &FeederConfig,
) -> Result<Vec<AggregatedPrice>> {
    if config.currency_pairs.is_empty() {
        return Ok(Vec::new());
    }

    // Fetch from all providers (each provider gets only the pairs it's configured for)
    let all_pairs: Vec<(String, String)> = config
        .currency_pairs
        .iter()
        .map(|p| (p.base.clone(), p.quote.clone()))
        .collect();

    let mut all_tickers: Vec<(String, HashMap<String, TickerPrice>)> = Vec::new();
    let mut all_candles: Vec<(String, HashMap<String, Vec<CandlePrice>>)> = Vec::new();

    for provider in providers {
        // Fetch tickers
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.get_ticker_prices(&all_pairs),
        )
        .await
        {
            Ok(Ok(tickers)) => {
                all_tickers.push((provider.name().to_string(), tickers));
            }
            Ok(Err(e)) => {
                tracing::warn!(provider = provider.name(), error = %e, "provider ticker fetch failed");
            }
            Err(_) => {
                tracing::warn!(
                    provider = provider.name(),
                    "provider ticker fetch timed out"
                );
            }
        }

        // Fetch candles
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.get_candle_prices(&all_pairs),
        )
        .await
        {
            Ok(Ok(candles)) => {
                if !candles.is_empty() {
                    all_candles.push((provider.name().to_string(), candles));
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(provider = provider.name(), error = %e, "provider candle fetch failed");
            }
            Err(_) => {
                tracing::warn!(
                    provider = provider.name(),
                    "provider candle fetch timed out"
                );
            }
        }
    }

    // Aggregate per pair, respecting per-pair provider config
    let mut results = Vec::new();

    for pair_config in &config.currency_pairs {
        let key = format!("{}/{}", pair_config.base, pair_config.quote);
        let threshold = config.deviation_for(&pair_config.base);

        // --- Try candle TVWAP first ---
        let mut candle_data: Vec<(f64, f64)> = Vec::new();
        for (provider_name, candle_map) in &all_candles {
            if !pair_config.providers.iter().any(|p| p == provider_name) {
                continue;
            }
            if let Some(candles) = candle_map.get(&key) {
                for c in candles {
                    if c.price > 0.0 && c.volume > 0.0 {
                        candle_data.push((c.price, c.volume));
                    }
                }
            }
        }

        if !candle_data.is_empty() {
            let (tvwap, total_volume) = compute_tvwap_fixed(&candle_data);
            if !tvwap.is_zero() {
                tracing::debug!(pair = %key, "using candle TVWAP");
                results.push(AggregatedPrice {
                    base: pair_config.base.clone(),
                    quote: pair_config.quote.clone(),
                    price: tvwap,
                    volume: total_volume,
                });
                continue;
            }
        }

        // --- Fall back to ticker VWAP ---
        let mut raw_prices: Vec<(f64, f64)> = Vec::new();
        for (provider_name, tickers) in &all_tickers {
            if !pair_config.providers.iter().any(|p| p == provider_name) {
                continue;
            }
            if let Some(ticker) = tickers.get(&key) {
                if ticker.price > 0.0 {
                    raw_prices.push((ticker.price, ticker.volume.max(1.0)));
                }
            }
        }

        if raw_prices.is_empty() {
            continue;
        }

        // Filter outliers using f64 σ-test (statistical operation, not pricing)
        let filtered = filter_deviations(&raw_prices, threshold);
        if filtered.is_empty() {
            continue;
        }

        // Compute VWAP in U256 fixed-point
        let (vwap, total_volume) = compute_vwap_fixed(&filtered);
        if !vwap.is_zero() {
            tracing::debug!(pair = %key, "using ticker VWAP (no candles available)");
            results.push(AggregatedPrice {
                base: pair_config.base.clone(),
                quote: pair_config.quote.clone(),
                price: vwap,
                volume: total_volume,
            });
        }
    }

    Ok(results)
}

/// Filters prices that deviate more than `threshold` standard deviations from the median.
///
/// Uses f64 for the statistical test only — this is a provider-level outlier filter,
/// not on-chain pricing arithmetic. The filtered set is then fed into U256 VWAP.
fn filter_deviations(prices: &[(f64, f64)], threshold: f64) -> Vec<(f64, f64)> {
    if prices.len() <= 1 {
        return prices.to_vec();
    }

    let mut sorted: Vec<f64> = prices.iter().map(|(p, _)| *p).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    let mean: f64 = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let variance: f64 =
        sorted.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / sorted.len() as f64;
    let std_dev = variance.sqrt();

    if std_dev < f64::EPSILON {
        return prices.to_vec();
    }

    prices
        .iter()
        .filter(|(p, _)| (p - median).abs() <= threshold * std_dev)
        .cloned()
        .collect()
}

/// Computes Time-Volume-Weighted Average Price (TVWAP) in U256 fixed-point at 1e18 scale.
///
/// Formula: `sum(close_i * volume_i) / sum(volume_i)` over candle bars.
///
/// Note: despite the "T" (time) in TVWAP, this computes volume-weighted average
/// over candles, not time-duration-weighted. Candle durations are assumed equal.
/// This matches Cosmos `oracle/price-feeder/oracle/oracle.go:ComputeTVWAP` which
/// also uses volume weighting only. True time-weighting would require candle
/// duration metadata not available from current providers.
///
/// Input: `(close_price_f64, volume_f64)` candle data from providers.
/// Each value is converted to U256 at 1e18 scale before accumulation.
/// Returns `(tvwap, total_volume)` both at 1e18 scale.
pub fn compute_tvwap_fixed(candles: &[(f64, f64)]) -> (U256, U256) {
    let mut price_volume_sum = U256::ZERO; // at 1e36 scale (1e18 * 1e18)
    let mut volume_sum = U256::ZERO; // at 1e18 scale

    for &(price, volume) in candles {
        let p = f64_to_u256(price);
        let v = f64_to_u256(volume);
        if let Some(pv) = p.checked_mul(v) {
            price_volume_sum = price_volume_sum.saturating_add(pv);
        }
        volume_sum = volume_sum.saturating_add(v);
    }

    if volume_sum.is_zero() {
        return (U256::ZERO, U256::ZERO);
    }

    let tvwap = price_volume_sum / volume_sum;
    (tvwap, volume_sum)
}

/// Computes VWAP in U256 fixed-point at 1e18 scale.
///
/// Input: filtered (price_f64, volume_f64) from providers.
/// Each is converted to U256 at 1e18 scale before accumulation.
/// Returns (vwap, total_volume) both at 1e18 scale.
fn compute_vwap_fixed(prices: &[(f64, f64)]) -> (U256, U256) {
    let mut price_volume_sum = U256::ZERO; // at 1e36 scale (1e18 * 1e18)
    let mut volume_sum = U256::ZERO; // at 1e18 scale

    for &(price, volume) in prices {
        let p = f64_to_u256(price);
        let v = f64_to_u256(volume);
        if let Some(pv) = p.checked_mul(v) {
            price_volume_sum = price_volume_sum.saturating_add(pv);
        }
        volume_sum = volume_sum.saturating_add(v);
    }

    if volume_sum.is_zero() {
        return (U256::ZERO, U256::ZERO);
    }

    // VWAP = price_volume_sum / volume_sum (result at 1e18 scale)
    let vwap = price_volume_sum / volume_sum;
    (vwap, volume_sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AccountConfig, ChainConfig, CurrencyPairConfig, FeederConfig, OracleConfig,
    };
    use crate::provider::mock::MockProvider;
    use outbe_primitives::units::{Units, SCALE_1E18};

    #[test]
    fn test_compute_vwap_fixed() {
        let prices = vec![(100.0, 1000.0), (200.0, 2000.0), (300.0, 3000.0)];
        let (vwap, volume) = compute_vwap_fixed(&prices);
        // VWAP = (100*1000 + 200*2000 + 300*3000) / (1000 + 2000 + 3000) ≈ 233.33
        let expected_vwap = U256::in_units(233u128); // floor(233.33) * 1e18
                                                     // Allow 1 unit tolerance due to fixed-point rounding
        assert!(vwap >= expected_vwap);
        assert!(vwap < expected_vwap + SCALE_1E18); // within 1.0

        let expected_volume = U256::in_units(6000u128);
        assert_eq!(volume, expected_volume);
    }

    #[test]
    fn test_compute_tvwap_fixed_basic() {
        // Three candles: close * volume weighted
        let candles = vec![(100.0, 3000.0), (102.0, 4000.0), (101.0, 3500.0)];
        let (tvwap, total_volume) = compute_tvwap_fixed(&candles);

        // Expected TVWAP = (100*3000 + 102*4000 + 101*3500) / (3000+4000+3500)
        //                = (300000 + 408000 + 353500) / 10500
        //                = 1061500 / 10500 ≈ 101.095238
        let expected_min = U256::in_units(101u128);
        let expected_max = U256::in_units(102u128);
        assert!(tvwap >= expected_min, "tvwap too low: {tvwap}");
        assert!(tvwap < expected_max, "tvwap too high: {tvwap}");

        let expected_volume = U256::in_units(10500u128);
        assert_eq!(total_volume, expected_volume);
    }

    #[test]
    fn test_compute_tvwap_fixed_single_candle() {
        let candles = vec![(50.0, 1000.0)];
        let (tvwap, volume) = compute_tvwap_fixed(&candles);
        assert_eq!(tvwap, f64_to_u256(50.0));
        assert_eq!(volume, f64_to_u256(1000.0));
    }

    #[test]
    fn test_compute_tvwap_fixed_empty() {
        let candles: Vec<(f64, f64)> = vec![];
        let (tvwap, volume) = compute_tvwap_fixed(&candles);
        assert_eq!(tvwap, U256::ZERO);
        assert_eq!(volume, U256::ZERO);
    }

    #[test]
    fn test_compute_tvwap_fixed_zero_volume() {
        let candles = vec![(100.0, 0.0), (200.0, 0.0)];
        let (tvwap, volume) = compute_tvwap_fixed(&candles);
        // f64_to_u256(0.0) == ZERO, so volume_sum stays zero
        assert_eq!(tvwap, U256::ZERO);
        assert_eq!(volume, U256::ZERO);
    }

    #[test]
    fn test_filter_deviations() {
        let prices = vec![(100.0, 1.0), (101.0, 1.0), (102.0, 1.0), (999.0, 1.0)];
        let filtered = filter_deviations(&prices, 2.0);
        assert!(filtered.len() < prices.len());
        assert!(filtered.iter().all(|(p, _)| *p < 500.0));
    }

    #[test]
    fn test_filter_identical_prices() {
        let prices = vec![(100.0, 1.0), (100.0, 1.0), (100.0, 1.0)];
        let filtered = filter_deviations(&prices, 2.0);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_f64_to_u256() {
        assert_eq!(f64_to_u256(1.0), SCALE_1E18);
        assert_eq!(f64_to_u256(0.5), U256::from(SCALE_1E18_U128 / 2));
        assert_eq!(f64_to_u256(0.0), U256::ZERO);
        assert_eq!(f64_to_u256(-1.0), U256::ZERO);
    }

    fn test_config(pairs: Vec<CurrencyPairConfig>) -> FeederConfig {
        FeederConfig {
            chain: ChainConfig {
                rpc_endpoint: "http://localhost:8545".into(),
                chain_id: 1,
                gasless_oracle_votes: false,
            },
            account: AccountConfig {
                private_key: "0xdead".into(),
                validator_address: "0x1111111111111111111111111111111111111111".into(),
            },
            oracle: OracleConfig {
                vote_period: 2,
                poll_interval_secs: 2,
            },
            currency_pairs: pairs,
            deviation_thresholds: vec![],
            provider_endpoints: vec![],
            health: None,
        }
    }

    /// Mock provider returns candles for known pairs → TVWAP should be used.
    #[tokio::test]
    async fn test_fetch_and_aggregate_uses_candle_tvwap() {
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(MockProvider::new())];
        let config = test_config(vec![CurrencyPairConfig {
            base: "ETH".into(),
            quote: "0xUSD".into(),
            chain_denom: None,
            providers: vec!["mock".into()],
        }]);

        let results = fetch_and_aggregate(&providers, &config).await.unwrap();
        assert_eq!(results.len(), 1);

        let agg = &results[0];
        assert_eq!(agg.base, "ETH");
        assert!(!agg.price.is_zero());

        // MockProvider candles: 2475.0 * 3000 + 2525.0 * 4000 + 2500.0 * 3500
        // = 7425000 + 10100000 + 8750000 = 26275000 / 10500 ≈ 2502.38
        // The TVWAP should be close to 2500 (within 1% = 25.0)
        let price_f64 = agg.price.to::<u128>() as f64 / SCALE_1E18_U128 as f64;
        assert!(
            (price_f64 - 2500.0).abs() < 25.0,
            "TVWAP {price_f64} not close to 2500"
        );
    }

    /// Provider with no candles falls back to ticker VWAP.
    #[tokio::test]
    async fn test_fetch_and_aggregate_fallback_to_ticker_vwap() {
        // Use a provider that returns tickers but no candles (Chainlink stub).
        use crate::provider::chainlink::ChainlinkProvider;

        // Chainlink returns nothing, so we also need mock for ticker data.
        // Instead, build a minimal provider that returns tickers but not candles.
        struct TickerOnlyProvider;

        #[async_trait::async_trait]
        impl Provider for TickerOnlyProvider {
            fn name(&self) -> &str {
                "ticker_only"
            }
            async fn get_ticker_prices(
                &self,
                pairs: &[(String, String)],
            ) -> eyre::Result<HashMap<String, TickerPrice>> {
                let mut m = HashMap::new();
                for (base, quote) in pairs {
                    let key = format!("{base}/{quote}");
                    if key == "ETH/0xUSD" {
                        m.insert(
                            key,
                            TickerPrice {
                                price: 2500.0,
                                volume: 5000.0,
                            },
                        );
                    }
                }
                Ok(m)
            }
            // get_candle_prices uses default → returns empty
        }

        let _ = ChainlinkProvider::new(); // silence unused import

        let providers: Vec<Box<dyn Provider>> = vec![Box::new(TickerOnlyProvider)];
        let config = test_config(vec![CurrencyPairConfig {
            base: "ETH".into(),
            quote: "0xUSD".into(),
            chain_denom: None,
            providers: vec!["ticker_only".into()],
        }]);

        let results = fetch_and_aggregate(&providers, &config).await.unwrap();
        assert_eq!(results.len(), 1);

        let agg = &results[0];
        let price_f64 = agg.price.to::<u128>() as f64 / SCALE_1E18_U128 as f64;
        assert!(
            (price_f64 - 2500.0).abs() < 1.0,
            "ticker VWAP {price_f64} not close to 2500"
        );
    }

    #[tokio::test]
    async fn test_feeder_provider_routing_filters_per_pair() {
        struct RoutingProvider {
            name: &'static str,
            coen_price: f64,
            eth_price: f64,
        }

        #[async_trait::async_trait]
        impl Provider for RoutingProvider {
            fn name(&self) -> &str {
                self.name
            }

            async fn get_ticker_prices(
                &self,
                pairs: &[(String, String)],
            ) -> eyre::Result<HashMap<String, TickerPrice>> {
                let mut prices = HashMap::new();
                for (base, quote) in pairs {
                    let key = format!("{base}/{quote}");
                    let price = match key.as_str() {
                        "COEN/0xUSD" => self.coen_price,
                        "ETH/0xUSD" => self.eth_price,
                        _ => continue,
                    };
                    prices.insert(
                        key,
                        TickerPrice {
                            price,
                            volume: 1_000.0,
                        },
                    );
                }
                Ok(prices)
            }
        }

        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(RoutingProvider {
                name: "provider_a",
                coen_price: 1.0,
                eth_price: 100.0,
            }),
            Box::new(RoutingProvider {
                name: "provider_b",
                coen_price: 9.0,
                eth_price: 2500.0,
            }),
        ];
        let config = test_config(vec![
            CurrencyPairConfig {
                base: "COEN".into(),
                quote: "0xUSD".into(),
                chain_denom: None,
                providers: vec!["provider_a".into()],
            },
            CurrencyPairConfig {
                base: "ETH".into(),
                quote: "0xUSD".into(),
                chain_denom: None,
                providers: vec!["provider_b".into()],
            },
        ]);

        let results = fetch_and_aggregate(&providers, &config).await.unwrap();
        assert_eq!(results.len(), 2);

        let coen = results.iter().find(|p| p.base == "COEN").unwrap();
        let eth = results.iter().find(|p| p.base == "ETH").unwrap();
        let coen_price = coen.price.to::<u128>() as f64 / SCALE_1E18_U128 as f64;
        let eth_price = eth.price.to::<u128>() as f64 / SCALE_1E18_U128 as f64;

        assert_eq!(coen.quote, "0xUSD");
        assert_eq!(eth.quote, "0xUSD");
        assert!(
            (coen_price - 1.0).abs() < f64::EPSILON,
            "COEN used the wrong provider price: {coen_price}"
        );
        assert!(
            (eth_price - 2500.0).abs() < f64::EPSILON,
            "ETH used the wrong provider price: {eth_price}"
        );
    }
}
