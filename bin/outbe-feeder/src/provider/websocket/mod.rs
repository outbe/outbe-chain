//! Exchange WebSocket transport shared by the streaming price providers.
//!
//! The transport owns connection, subscription, heartbeat, reconnect and
//! in-memory market-data caching. Exchange-specific code is deliberately kept
//! to deterministic symbol mapping, subscription JSON and message decoding.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use eyre::{eyre, Result};
use futures::{SinkExt as _, StreamExt as _};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{CandlePrice, Provider, TickerPrice};
use crate::config::ApiProviderConfig;
use crate::fixed::FixedValue;
use decode::{
    clear_routes, decompress_gzip, normalize_symbol, process_payload, subscription_messages,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
const STALE_CONNECTION_AFTER: Duration = Duration::from_secs(60);
const STABLE_SESSION_AFTER: Duration = Duration::from_secs(60);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const MAX_CANDLES_PER_PAIR: usize = 60;
const MAX_DECOMPRESSED_MESSAGE_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExchangeKind {
    Binance,
    Kraken,
    Okx,
    Gate,
    Huobi,
    Mexc,
    Coinbase,
}

impl ExchangeKind {
    pub(crate) fn for_name(name: &str) -> Option<Self> {
        match name {
            "binance" => Some(Self::Binance),
            "kraken" => Some(Self::Kraken),
            "okx" => Some(Self::Okx),
            "gate" => Some(Self::Gate),
            "huobi" => Some(Self::Huobi),
            "mexc" => Some(Self::Mexc),
            "coinbase" => Some(Self::Coinbase),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Kraken => "kraken",
            Self::Okx => "okx",
            Self::Gate => "gate",
            Self::Huobi => "huobi",
            Self::Mexc => "mexc",
            Self::Coinbase => "coinbase",
        }
    }

    const fn default_url(self) -> &'static str {
        match self {
            Self::Binance => "wss://stream.binance.com:9443/ws",
            Self::Kraken => "wss://ws.kraken.com",
            Self::Okx => "wss://ws.okx.com:8443/ws/v5/public",
            Self::Gate => "wss://api.gateio.ws/ws/v4/",
            Self::Huobi => "wss://api-aws.huobi.pro/ws",
            Self::Mexc => "wss://wbs-api.mexc.com/ws",
            Self::Coinbase => "wss://ws-feed.exchange.coinbase.com",
        }
    }

    const fn default_path(self) -> &'static str {
        match self {
            Self::Binance => "/ws",
            Self::Kraken | Self::Coinbase => "",
            Self::Okx => "/ws/v5/public",
            Self::Gate => "/ws/v4/",
            Self::Huobi => "/ws",
            Self::Mexc => "/ws",
        }
    }

    fn websocket_url(self, endpoint: Option<&ApiProviderConfig>) -> String {
        let configured = endpoint
            .map(|value| value.websocket.trim())
            .unwrap_or_default();
        if configured.is_empty() {
            return self.default_url().to_owned();
        }
        if configured.starts_with("ws://") || configured.starts_with("wss://") {
            return configured.to_owned();
        }
        format!("wss://{configured}{}", self.default_path())
    }

    /// The exchange market for `base/quote`, both written in the exchange's
    /// own symbols: the config says which market prices a pair, the code
    /// never substitutes one. Addresses and ISO codes are not exchange symbols.
    fn pair_route(self, base: &str, quote: &str) -> Option<PairRoute> {
        if [base, quote]
            .iter()
            .any(|asset| asset.starts_with("0x") || is_iso_symbol(asset))
        {
            return None;
        }
        let provider_base = if self == Self::Kraken && base.eq_ignore_ascii_case("BTC") {
            "XBT".to_owned()
        } else {
            base.to_owned()
        };
        let provider_quote = quote.to_owned();
        let provider_symbol = match self {
            Self::Kraken => format!("{provider_base}/{provider_quote}"),
            Self::Okx | Self::Coinbase => format!("{provider_base}-{provider_quote}"),
            Self::Gate => format!("{provider_base}_{provider_quote}"),
            Self::Huobi => format!("{provider_base}{provider_quote}").to_ascii_lowercase(),
            Self::Binance | Self::Mexc => {
                format!("{provider_base}{provider_quote}").to_ascii_uppercase()
            }
        };
        Some(PairRoute {
            key: format!("{base}/{quote}"),
            fallback_key: format!("{provider_base}/{provider_quote}"),
            fallback_pair: (provider_base, provider_quote),
            provider_symbol,
        })
    }

    pub(crate) fn supports_any(self, pairs: &[(String, String)]) -> bool {
        pairs
            .iter()
            .any(|(base, quote)| self.pair_route(base, quote).is_some())
    }
}

/// An exchange market is written in the exchange's own symbols. The code
/// substitutes nothing for an ISO code: which dollar market prices the pair,
/// USDT, USDC or USD, is the operator's choice.
pub(crate) fn validate_exchange_markets(endpoint: &ApiProviderConfig) -> Result<()> {
    if ExchangeKind::for_name(&endpoint.name).is_none() {
        return Ok(());
    }
    for market in &endpoint.markets {
        if is_iso_symbol(&market.quote) {
            return Err(eyre!(
                "exchange '{}' market quote '{}' is an ISO code; write the exchange's market, such as USDT, USDC or USD",
                endpoint.name,
                market.quote
            ));
        }
    }
    Ok(())
}

fn is_iso_symbol(value: &str) -> bool {
    value
        .parse::<u16>()
        .is_ok_and(|code| (1..=999).contains(&code))
}

#[derive(Clone, Debug)]
struct PairRoute {
    key: String,
    fallback_key: String,
    fallback_pair: (String, String),
    provider_symbol: String,
}

impl PairRoute {
    fn matches(&self, symbol: &str) -> bool {
        normalize_symbol(&self.provider_symbol) == normalize_symbol(symbol)
    }
}

#[derive(Debug, Default)]
struct MarketCache {
    tickers: HashMap<String, TickerPrice>,
    ticker_updated: HashMap<String, Instant>,
    candles: HashMap<String, Vec<CandlePrice>>,
    candle_updated: HashMap<String, Instant>,
}

type SharedCache = Arc<RwLock<MarketCache>>;

/// A provider facade backed by live WebSocket data. It uses its REST adapter
/// for each requested market that has no fresh stream value. This includes a
/// market that has not appeared in the stream yet. It also includes a value
/// older than `STALE_CONNECTION_AFTER` and a value cleared after a disconnect.
pub(crate) struct StreamingProvider {
    kind: ExchangeKind,
    fallback: Box<dyn Provider>,
    cache: SharedCache,
    routes: Vec<PairRoute>,
    allowed_keys: HashSet<String>,
    actors: Vec<JoinHandle<()>>,
}

impl StreamingProvider {
    pub(crate) fn new(
        kind: ExchangeKind,
        endpoint: Option<&ApiProviderConfig>,
        pairs: &[(String, String)],
        fallback: Box<dyn Provider>,
    ) -> Option<Self> {
        let routes = pairs
            .iter()
            .filter_map(|(base, quote)| kind.pair_route(base, quote))
            .collect::<Vec<_>>();
        if routes.is_empty() {
            return None;
        }

        let url = kind.websocket_url(endpoint);
        let provider_routes = routes.clone();
        let allowed_keys = routes.iter().map(|route| route.key.clone()).collect();
        let cache = Arc::new(RwLock::new(MarketCache::default()));
        // MEXC permits at most 30 subscriptions on one connection. Each route
        // consumes a mini-ticker and a candle subscription.
        let route_groups = if kind == ExchangeKind::Mexc {
            routes.chunks(15).map(<[_]>::to_vec).collect::<Vec<_>>()
        } else {
            vec![routes]
        };
        let actors = route_groups
            .into_iter()
            .map(|routes| {
                let actor_cache = Arc::clone(&cache);
                let url = url.clone();
                tokio::spawn(async move {
                    run_connection_loop(kind, url, routes, actor_cache).await;
                })
            })
            .collect();
        Some(Self {
            kind,
            fallback,
            cache,
            routes: provider_routes,
            allowed_keys,
            actors,
        })
    }

    fn cached_tickers(&self, pairs: &[(String, String)]) -> HashMap<String, TickerPrice> {
        let cache = self.cache.read().unwrap_or_else(|error| error.into_inner());
        self.fresh(pairs, &cache.tickers, &cache.ticker_updated)
    }

    fn cached_candles(&self, pairs: &[(String, String)]) -> HashMap<String, Vec<CandlePrice>> {
        let cache = self.cache.read().unwrap_or_else(|error| error.into_inner());
        self.fresh(pairs, &cache.candles, &cache.candle_updated)
    }

    /// The allowed pairs whose cached value is younger than
    /// [`STALE_CONNECTION_AFTER`].
    fn fresh<T: Clone>(
        &self,
        pairs: &[(String, String)],
        values: &HashMap<String, T>,
        updated: &HashMap<String, Instant>,
    ) -> HashMap<String, T> {
        pairs
            .iter()
            .map(|(base, quote)| format!("{base}/{quote}"))
            .filter(|key| self.allowed_keys.contains(key))
            .filter_map(|key| {
                updated
                    .get(&key)
                    .is_some_and(|updated| updated.elapsed() < STALE_CONNECTION_AFTER)
                    .then(|| values.get(&key).cloned())
                    .flatten()
                    .map(|value| (key, value))
            })
            .collect()
    }

    /// Routes of the allowed pairs `served` did not cover, with the fallback
    /// markets to read for them.
    fn unserved_routes<T>(
        &self,
        pairs: &[(String, String)],
        served: &HashMap<String, T>,
    ) -> (Vec<&PairRoute>, Vec<(String, String)>) {
        let routes = pairs
            .iter()
            .map(|(base, quote)| format!("{base}/{quote}"))
            .filter(|key| self.allowed_keys.contains(key) && !served.contains_key(key))
            .filter_map(|key| self.routes.iter().find(|route| route.key == key))
            .collect::<Vec<_>>();
        let fallback_pairs = routes
            .iter()
            .map(|route| route.fallback_pair.clone())
            .collect();
        (routes, fallback_pairs)
    }
}

/// Copies the fallback's value of every route into `values` under the
/// route's own key.
fn merge_fallback<T>(
    values: &mut HashMap<String, T>,
    routes: &[&PairRoute],
    fallback: &HashMap<String, T>,
) where
    T: Clone,
{
    for route in routes {
        if let Some(value) = fallback.get(&route.fallback_key).cloned() {
            values.insert(route.key.clone(), value);
        }
    }
}

impl Drop for StreamingProvider {
    fn drop(&mut self) {
        for actor in &self.actors {
            actor.abort();
        }
    }
}

#[async_trait]
impl Provider for StreamingProvider {
    fn name(&self) -> &str {
        self.kind.name()
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut values = self.cached_tickers(pairs);
        let (routes, fallback_pairs) = self.unserved_routes(pairs, &values);
        if !routes.is_empty() {
            let fallback = self.fallback.get_ticker_prices(&fallback_pairs).await?;
            merge_fallback(&mut values, &routes, &fallback);
        }
        Ok(values)
    }

    async fn get_candle_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, Vec<CandlePrice>>> {
        let mut values = self.cached_candles(pairs);
        let (routes, fallback_pairs) = self.unserved_routes(pairs, &values);
        if !routes.is_empty() {
            let fallback = self.fallback.get_candle_prices(&fallback_pairs).await?;
            merge_fallback(&mut values, &routes, &fallback);
        }
        Ok(values)
    }
}

async fn run_connection_loop(
    kind: ExchangeKind,
    url: String,
    routes: Vec<PairRoute>,
    cache: SharedCache,
) {
    let mut reconnect_delay = Duration::from_secs(1);
    loop {
        let mut received_market_data = false;
        let mut connected_for = Duration::ZERO;
        match connect_async(&url).await {
            Ok((socket, _)) => {
                tracing::info!(provider = kind.name(), url, "exchange websocket connected");
                let connected_at = Instant::now();
                if let Err(error) = drive_connection(kind, socket, &routes, &cache).await {
                    connected_for = connected_at.elapsed();
                    tracing::warn!(provider = kind.name(), %error, "exchange websocket disconnected");
                    received_market_data = routes_have_cached_data(&cache, &routes);
                    clear_routes(&cache, &routes);
                }
            }
            Err(error) => {
                tracing::warn!(provider = kind.name(), url, %error, "exchange websocket connection failed");
            }
        }
        reconnect_delay =
            reconnect_delay_after_session(reconnect_delay, received_market_data, connected_for);
        sleep(reconnect_delay).await;
        reconnect_delay = reconnect_delay
            .checked_mul(2)
            .unwrap_or(MAX_RECONNECT_DELAY)
            .min(MAX_RECONNECT_DELAY);
    }
}

fn routes_have_cached_data(cache: &SharedCache, routes: &[PairRoute]) -> bool {
    cache.read().is_ok_and(|cache| {
        routes.iter().any(|route| {
            cache.ticker_updated.contains_key(&route.key)
                || cache.candle_updated.contains_key(&route.key)
        })
    })
}

fn reconnect_delay_after_session(
    current: Duration,
    received_market_data: bool,
    connected_for: Duration,
) -> Duration {
    if received_market_data || connected_for >= STABLE_SESSION_AFTER {
        Duration::from_secs(1)
    } else {
        current
    }
}

async fn drive_connection<S>(
    kind: ExchangeKind,
    socket: tokio_tungstenite::WebSocketStream<S>,
    routes: &[PairRoute],
    cache: &SharedCache,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = socket.split();
    for subscription in subscription_messages(kind, routes)? {
        sink.send(Message::Text(subscription.into())).await?;
    }

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut last_seen = Instant::now();

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let message = incoming.ok_or_else(|| eyre!("websocket stream ended"))??;
                last_seen = Instant::now();
                match message {
                    Message::Text(text) => {
                        if text.as_str() == "pong" {
                            continue;
                        }
                        match process_payload(kind, text.as_bytes(), routes, cache) {
                            Ok(Some(reply)) => sink.send(Message::Text(reply.into())).await?,
                            Ok(None) => {}
                            Err(error) => tracing::warn!(provider = kind.name(), %error, "ignored malformed websocket payload"),
                        }
                    }
                    Message::Binary(bytes) => {
                        let payload = if kind == ExchangeKind::Huobi {
                            decompress_gzip(&bytes).unwrap_or_else(|_| bytes.to_vec())
                        } else {
                            bytes.to_vec()
                        };
                        match process_payload(kind, &payload, routes, cache) {
                            Ok(Some(reply)) => sink.send(Message::Text(reply.into())).await?,
                            Ok(None) => {}
                            Err(error) => tracing::warn!(provider = kind.name(), %error, "ignored malformed websocket payload"),
                        }
                    }
                    Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
                    Message::Pong(_) => {}
                    Message::Close(frame) => return Err(eyre!("remote closed websocket: {frame:?}")),
                    Message::Frame(_) => {}
                }
            }
            _ = heartbeat.tick() => {
                if last_seen.elapsed() >= STALE_CONNECTION_AFTER {
                    return Err(eyre!("websocket received no data or pong for {STALE_CONNECTION_AFTER:?}"));
                }
                if kind == ExchangeKind::Okx {
                    sink.send(Message::Text("ping".into())).await?;
                } else if kind == ExchangeKind::Mexc {
                    sink.send(Message::Text(r#"{"method":"PING"}"#.into())).await?;
                } else {
                    sink.send(Message::Ping(Vec::new().into())).await?;
                }
            }
        }
    }
}

mod decode;

#[cfg(test)]
mod tests;
