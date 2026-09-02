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
//! Provider decimals are ingested and aggregated entirely as deterministic FP18
//! integers. The final pair-specific conversion emits COEN/ISO at `1e6` and
//! leaves generic pairs at `1e18`.

use crate::config::{is_iso_symbol, FeederConfig};
use crate::fixed::FixedValue;
use crate::provider::{CandlePrice, Provider, TickerPrice};
use alloy_primitives::{aliases::U1024, U256, U512};
use eyre::Result;
use outbe_primitives::units::SCALE_1E18;
use std::collections::HashMap;

/// Aggregated price/volume for a single pair in that pair's registered scale.
#[derive(Debug, Clone)]
pub struct AggregatedPrice {
    pub base: String,
    pub quote: String,
    /// VWAP price: six decimals for COEN/ISO, existing decimal18 otherwise.
    pub price: U256,
    /// Total volume: COEN units for COEN/ISO, existing decimal18 otherwise.
    pub volume: U256,
}

fn is_coen_iso_pair(base: &str, quote: &str) -> bool {
    base.eq_ignore_ascii_case("COEN") && is_iso_symbol(quote)
}

const SCALE_1E12: U256 = U256::from_limbs([1_000_000_000_000u64, 0, 0, 0]);

/// Convert one FP18 aggregate to the pair's wire scale. A positive COEN/ISO
/// price below one `1e6` minor unit is rejected. Real zero volume remains zero;
/// a positive sub-minor COEN volume becomes one minor unit.
fn finalize_pair_value(is_coen_iso: bool, price: U256, volume: U256) -> Option<(U256, U256)> {
    if !is_coen_iso {
        return (!price.is_zero()).then_some((price, volume));
    }
    let price = price / SCALE_1E12;
    if price.is_zero() {
        return None;
    }
    let volume = if volume.is_zero() {
        U256::ZERO
    } else {
        (volume / SCALE_1E12).max(U256::ONE)
    };
    Some((price, volume))
}

/// FP18 weighted average. A zero-volume observation uses one whole unit only
/// as an internal weight; its published volume remains zero.
fn compute_weighted(prices: &[(FixedValue, FixedValue)]) -> Result<Option<(U256, U256)>> {
    let mut price_volume_sum = U1024::ZERO;
    let mut weight_sum = U512::ZERO;
    let mut volume_sum = U512::ZERO;

    for &(price, volume) in prices {
        let price = price.raw();
        if price.is_zero() {
            continue;
        }
        let volume = volume.raw();
        let weight = if volume.is_zero() { SCALE_1E18 } else { volume };
        let weighted_price = U512::from(price)
            .checked_mul(U512::from(weight))
            .ok_or_else(|| eyre::eyre!("price-volume product overflow"))?;
        price_volume_sum = price_volume_sum
            .checked_add(U1024::from(weighted_price))
            .ok_or_else(|| eyre::eyre!("price-volume sum overflow"))?;
        weight_sum = weight_sum
            .checked_add(U512::from(weight))
            .ok_or_else(|| eyre::eyre!("weight sum overflow"))?;
        volume_sum = volume_sum
            .checked_add(U512::from(volume))
            .ok_or_else(|| eyre::eyre!("volume sum overflow"))?;
    }

    if weight_sum.is_zero() {
        return Ok(None);
    }
    let price = narrow_u1024(price_volume_sum / U1024::from(weight_sum), "weighted price")?;
    let volume = narrow_u512(volume_sum, "volume sum")?;
    Ok(Some((price, volume)))
}

/// Fetches prices from providers, filters outliers, and computes the best
/// available weighted average price.
///
/// Strategy per pair:
/// 1. If any configured provider returns candle data -> compute TVWAP.
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
        let is_coen_iso = is_coen_iso_pair(&pair_config.base, &pair_config.quote);

        // --- Try candle TVWAP first ---
        let mut candle_data: Vec<(FixedValue, FixedValue)> = Vec::new();
        for (provider_name, candle_map) in &all_candles {
            if !pair_config.providers.iter().any(|p| p == provider_name) {
                continue;
            }
            if let Some(candles) = candle_map.get(&key) {
                for c in candles {
                    if !c.price.is_zero() && !c.volume.is_zero() {
                        candle_data.push((c.price, c.volume));
                    }
                }
            }
        }

        if !candle_data.is_empty() {
            if let Some((tvwap, total_volume)) = compute_weighted(&candle_data)?
                .and_then(|(price, volume)| finalize_pair_value(is_coen_iso, price, volume))
            {
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
        }

        // --- Fall back to ticker VWAP ---
        let mut raw_prices: Vec<(FixedValue, FixedValue)> = Vec::new();
        for (provider_name, tickers) in &all_tickers {
            if !pair_config.providers.iter().any(|p| p == provider_name) {
                continue;
            }
            if let Some(ticker) = tickers.get(&key) {
                if !ticker.price.is_zero() {
                    let volume = if is_coen_iso {
                        ticker.volume
                    } else {
                        ticker.volume.max(FixedValue::from_raw(SCALE_1E18))
                    };
                    raw_prices.push((ticker.price, volume));
                }
            }
        }

        if raw_prices.is_empty() {
            continue;
        }

        let filtered = filter_deviations(&raw_prices, threshold)?;
        if filtered.is_empty() {
            continue;
        }

        let Some((vwap, total_volume)) = compute_weighted(&filtered)?
            .and_then(|(price, volume)| finalize_pair_value(is_coen_iso, price, volume))
        else {
            continue;
        };
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

/// Filters prices that deviate more than `threshold` standard deviations from
/// the median. Prices, threshold, mean, variance and square root are all
/// deterministic integers; the threshold is dimensionless FP18.
fn filter_deviations(
    prices: &[(FixedValue, FixedValue)],
    threshold: FixedValue,
) -> Result<Vec<(FixedValue, FixedValue)>> {
    if prices.len() <= 1 {
        return Ok(prices.to_vec());
    }

    let mut sorted: Vec<U256> = prices.iter().map(|(price, _)| price.raw()).collect();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];

    let sum = sorted.iter().try_fold(U512::ZERO, |sum, price| {
        sum.checked_add(U512::from(*price))
            .ok_or_else(|| eyre::eyre!("deviation mean sum overflow"))
    })?;
    let mean = narrow_u512(sum / U512::from(sorted.len()), "deviation mean")?;
    let sum_sq = sorted.iter().try_fold(U1024::ZERO, |sum, price| {
        let deviation = price.abs_diff(mean);
        let wide = U1024::from(deviation);
        sum.checked_add(wide * wide)
            .ok_or_else(|| eyre::eyre!("deviation square sum overflow"))
    })?;
    let variance = sum_sq / U1024::from(sorted.len());
    let std_dev = narrow_u1024(isqrt_u1024(variance), "deviation standard deviation")?;

    if std_dev.is_zero() {
        return Ok(prices.to_vec());
    }

    let allowed = U512::from(threshold.raw()) * U512::from(std_dev) / U512::from(SCALE_1E18);

    Ok(prices
        .iter()
        .filter(|(price, _)| U512::from(price.raw().abs_diff(median)) <= allowed)
        .cloned()
        .collect())
}

fn narrow_u512(value: U512, label: &'static str) -> Result<U256> {
    if value > U512::from(U256::MAX) {
        return Err(eyre::eyre!("{label} exceeds U256"));
    }
    Ok(value.wrapping_to::<U256>())
}

fn narrow_u1024(value: U1024, label: &'static str) -> Result<U256> {
    if value > U1024::from(U256::MAX) {
        return Err(eyre::eyre!("{label} exceeds U256"));
    }
    Ok(value.wrapping_to::<U256>())
}

fn isqrt_u1024(n: U1024) -> U1024 {
    if n.is_zero() {
        return U1024::ZERO;
    }
    if n == U1024::ONE {
        return U1024::ONE;
    }
    let mut x = n;
    let mut y = (x >> 1) + U1024::ONE;
    while y < x {
        x = y;
        y = (x + n / x) >> 1;
    }
    x
}

#[cfg(test)]
fn compute_tvwap_fixed(candles: &[(FixedValue, FixedValue)]) -> Result<(U256, U256)> {
    Ok(compute_weighted(candles)?.unwrap_or((U256::ZERO, U256::ZERO)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AccountConfig, ChainConfig, CurrencyPairConfig, FeederConfig, OracleConfig,
    };
    use crate::provider::mock::MockProvider;
    use outbe_primitives::units::SCALE_1E18;

    const COEN_ISO_SCALE: u128 = 1_000_000;

    fn fp(value: &str) -> FixedValue {
        FixedValue::parse(value).unwrap()
    }

    struct CoenTickerProvider {
        quote: &'static str,
        price: FixedValue,
        volume: FixedValue,
    }

    #[async_trait::async_trait]
    impl Provider for CoenTickerProvider {
        fn name(&self) -> &str {
            "coen_ticker"
        }

        async fn get_ticker_prices(
            &self,
            pairs: &[(String, String)],
        ) -> eyre::Result<HashMap<String, TickerPrice>> {
            let mut prices = HashMap::new();
            let key = format!("COEN/{}", self.quote);
            if pairs
                .iter()
                .any(|(base, quote)| base == "COEN" && quote == self.quote)
            {
                prices.insert(
                    key,
                    TickerPrice {
                        price: self.price,
                        volume: self.volume,
                    },
                );
            }
            Ok(prices)
        }
    }

    async fn aggregate_coen_iso_ticker(
        quote: &'static str,
        price: &str,
        volume: &str,
    ) -> Vec<AggregatedPrice> {
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(CoenTickerProvider {
            quote,
            price: fp(price),
            volume: fp(volume),
        })];
        let config = test_config(vec![CurrencyPairConfig {
            base: "COEN".into(),
            quote: quote.into(),
            providers: vec!["coen_ticker".into()],
        }]);
        fetch_and_aggregate(&providers, &config).await.unwrap()
    }

    #[test]
    fn test_compute_vwap_fixed() {
        let prices = vec![
            (fp("100"), fp("1000")),
            (fp("200"), fp("2000")),
            (fp("300"), fp("3000")),
        ];
        let (vwap, volume) = compute_weighted(&prices).unwrap().unwrap();
        // VWAP = (100*1000 + 200*2000 + 300*3000) / (1000 + 2000 + 3000) ~= 233.33
        let expected_vwap = U256::from(233u128) * SCALE_1E18;
        // Allow 1 unit tolerance due to fixed-point rounding
        assert!(vwap >= expected_vwap);
        assert!(vwap < expected_vwap + SCALE_1E18); // within 1.0

        let expected_volume = U256::from(6000u128) * SCALE_1E18;
        assert_eq!(volume, expected_volume);
    }

    #[test]
    fn test_compute_tvwap_fixed_basic() {
        // Three candles: close * volume weighted
        let candles = vec![
            (fp("100"), fp("3000")),
            (fp("102"), fp("4000")),
            (fp("101"), fp("3500")),
        ];
        let (tvwap, total_volume) = compute_tvwap_fixed(&candles).unwrap();

        // Expected TVWAP = (100*3000 + 102*4000 + 101*3500) / (3000+4000+3500)
        //                = (300000 + 408000 + 353500) / 10500
        //                = 1061500 / 10500 ~= 101.095238
        let expected_min = U256::from(101u128) * SCALE_1E18;
        let expected_max = U256::from(102u128) * SCALE_1E18;
        assert!(tvwap >= expected_min, "tvwap too low: {tvwap}");
        assert!(tvwap < expected_max, "tvwap too high: {tvwap}");

        let expected_volume = U256::from(10500u128) * SCALE_1E18;
        assert_eq!(total_volume, expected_volume);
    }

    #[test]
    fn test_compute_tvwap_fixed_single_candle() {
        let candles = vec![(fp("50"), fp("1000"))];
        let (tvwap, volume) = compute_tvwap_fixed(&candles).unwrap();
        assert_eq!(tvwap, fp("50").raw());
        assert_eq!(volume, fp("1000").raw());
    }

    #[test]
    fn test_compute_tvwap_fixed_empty() {
        let candles: Vec<(FixedValue, FixedValue)> = vec![];
        let (tvwap, volume) = compute_tvwap_fixed(&candles).unwrap();
        assert_eq!(tvwap, U256::ZERO);
        assert_eq!(volume, U256::ZERO);
    }

    #[test]
    fn test_compute_tvwap_fixed_zero_volume() {
        let candles = vec![(fp("100"), fp("0")), (fp("200"), fp("0"))];
        let (tvwap, volume) = compute_tvwap_fixed(&candles).unwrap();
        assert_eq!(tvwap, fp("150").raw());
        assert_eq!(volume, U256::ZERO);
    }

    #[test]
    fn test_filter_deviations() {
        let prices = vec![
            (fp("100"), fp("1")),
            (fp("101"), fp("1")),
            (fp("102"), fp("1")),
            (fp("999"), fp("1")),
        ];
        let filtered = filter_deviations(&prices, fp("2")).unwrap();
        assert!(filtered.len() < prices.len());
        assert!(filtered.iter().all(|(p, _)| *p < fp("500")));
    }

    #[test]
    fn test_filter_identical_prices() {
        let prices = vec![
            (fp("100"), fp("1")),
            (fp("100"), fp("1")),
            (fp("100"), fp("1")),
        ];
        let filtered = filter_deviations(&prices, fp("2")).unwrap();
        assert_eq!(filtered.len(), 3);
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

    /// Mock provider returns candles for known pairs -> TVWAP should be used.
    #[tokio::test]
    async fn test_fetch_and_aggregate_uses_candle_tvwap() {
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(MockProvider::new())];
        let config = test_config(vec![CurrencyPairConfig {
            base: "ETH".into(),
            quote: "840".into(),
            providers: vec!["mock".into()],
        }]);

        let results = fetch_and_aggregate(&providers, &config).await.unwrap();
        assert_eq!(results.len(), 1);

        let agg = &results[0];
        assert_eq!(agg.base, "ETH");
        assert!(!agg.price.is_zero());

        // MockProvider candles: 2475.0 * 3000 + 2525.0 * 4000 + 2500.0 * 3500
        // = 7425000 + 10100000 + 8750000 = 26275000 / 10500 ~= 2502.38
        // The TVWAP should be close to 2500 (within 1% = 25.0)
        let lower = U256::from(2_475u64) * SCALE_1E18;
        let upper = U256::from(2_525u64) * SCALE_1E18;
        assert!(
            agg.price > lower && agg.price < upper,
            "TVWAP {}",
            agg.price
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
                    if key == "ETH/840" {
                        m.insert(
                            key,
                            TickerPrice {
                                price: fp("2500"),
                                volume: fp("5000"),
                            },
                        );
                    }
                }
                Ok(m)
            }
            // get_candle_prices uses default -> returns empty
        }

        let _ = ChainlinkProvider::new(); // silence unused import

        let providers: Vec<Box<dyn Provider>> = vec![Box::new(TickerOnlyProvider)];
        let config = test_config(vec![CurrencyPairConfig {
            base: "ETH".into(),
            quote: "840".into(),
            providers: vec!["ticker_only".into()],
        }]);

        let results = fetch_and_aggregate(&providers, &config).await.unwrap();
        assert_eq!(results.len(), 1);

        let agg = &results[0];
        assert_eq!(agg.price, U256::from(2_500u64) * SCALE_1E18);
    }

    #[tokio::test]
    async fn test_feeder_provider_routing_filters_per_pair() {
        struct RoutingProvider {
            name: &'static str,
            coen_price: FixedValue,
            eth_price: FixedValue,
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
                        "COEN/840" => self.coen_price,
                        "ETH/840" => self.eth_price,
                        _ => continue,
                    };
                    prices.insert(
                        key,
                        TickerPrice {
                            price,
                            volume: fp("1000"),
                        },
                    );
                }
                Ok(prices)
            }
        }

        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(RoutingProvider {
                name: "provider_a",
                coen_price: fp("1"),
                eth_price: fp("100"),
            }),
            Box::new(RoutingProvider {
                name: "provider_b",
                coen_price: fp("9"),
                eth_price: fp("2500"),
            }),
        ];
        let config = test_config(vec![
            CurrencyPairConfig {
                base: "COEN".into(),
                quote: "840".into(),
                providers: vec!["provider_a".into()],
            },
            CurrencyPairConfig {
                base: "ETH".into(),
                quote: "840".into(),
                providers: vec!["provider_b".into()],
            },
        ]);

        let results = fetch_and_aggregate(&providers, &config).await.unwrap();
        assert_eq!(results.len(), 2);

        let coen = results.iter().find(|p| p.base == "COEN").unwrap();
        let eth = results.iter().find(|p| p.base == "ETH").unwrap();

        assert_eq!(coen.quote, "840");
        assert_eq!(eth.quote, "840");
        assert_eq!(coen.price, U256::from(COEN_ISO_SCALE));
        assert_eq!(
            coen.volume,
            U256::from(1_000u64) * U256::from(COEN_ISO_SCALE)
        );
        assert_eq!(eth.price, U256::from(2_500u64) * SCALE_1E18);
        assert_eq!(eth.volume, U256::from(1_000u64) * SCALE_1E18);
    }

    #[tokio::test]
    async fn test_coen_iso_rejects_positive_price_below_one_price_unit() {
        assert!(aggregate_coen_iso_ticker("840", "0.0000004", "1")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn test_coen_iso_clamps_positive_subunit_volume_to_one_unit() {
        let prices = aggregate_coen_iso_ticker("840", "1", "0.0000004").await;
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].price, U256::from(COEN_ISO_SCALE));
        assert_eq!(prices[0].volume, U256::ONE);
    }

    #[tokio::test]
    async fn test_coen_iso_preserves_real_zero_volume() {
        let prices = aggregate_coen_iso_ticker("840", "1", "0").await;
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].price, U256::from(COEN_ISO_SCALE));
        assert_eq!(prices[0].volume, U256::ZERO);
    }

    #[tokio::test]
    async fn test_every_coen_iso_market_uses_six_decimal_price_and_volume() {
        let prices = aggregate_coen_iso_ticker("999", "1.25", "2.5").await;
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].quote, "999");
        assert_eq!(prices[0].price, U256::from(1_250_000u64));
        assert_eq!(prices[0].volume, U256::from(2_500_000u64));
    }
}
