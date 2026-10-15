use super::decode::{insert_candle, process_payload, subscription_messages};
use super::*;
use crate::fixed::FixedValue;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;

struct EmptyFallback;

#[async_trait]
impl Provider for EmptyFallback {
    fn name(&self) -> &str {
        "binance"
    }

    async fn get_ticker_prices(
        &self,
        _pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        Ok(HashMap::new())
    }
}

fn route() -> Vec<PairRoute> {
    vec![PairRoute {
        key: "ETH/USDT".to_owned(),
        fallback_key: "ETH/USDT".to_owned(),
        fallback_pair: ("ETH".to_owned(), "USDT".to_owned()),
        provider_symbol: "ETHUSDT".to_owned(),
    }]
}

#[test]
fn every_exchange_builds_valid_market_subscriptions() {
    for kind in [
        ExchangeKind::Binance,
        ExchangeKind::Kraken,
        ExchangeKind::Okx,
        ExchangeKind::Gate,
        ExchangeKind::Huobi,
        ExchangeKind::Mexc,
        ExchangeKind::Coinbase,
    ] {
        let messages = subscription_messages(kind, &route()).unwrap();
        assert!(!messages.is_empty());
        assert!(messages
            .iter()
            .all(|message| serde_json::from_str::<Value>(message).is_ok()));
    }

    let mexc = subscription_messages(ExchangeKind::Mexc, &route()).unwrap();
    assert!(mexc[0].contains("spot@public.miniTicker.v3.api.pb@ETHUSDT@UTC+8"));
    assert!(mexc[0].contains("spot@public.kline.v3.api.pb@ETHUSDT@Min1"));
}

#[test]
fn exchange_markets_are_written_in_exchange_symbols() {
    // No ISO code is substituted: the config names the exchange market.
    assert!(ExchangeKind::Binance.pair_route("COEN", "840").is_none());
    assert!(ExchangeKind::Kraken.pair_route("840", "BTC").is_none());
    assert!(ExchangeKind::Coinbase.pair_route("ETH", "978").is_none());
    let kraken = ExchangeKind::Kraken.pair_route("BTC", "USD").unwrap();
    assert_eq!(kraken.provider_symbol, "XBT/USD");
    assert_eq!(kraken.fallback_pair, ("XBT".to_owned(), "USD".to_owned()));
}

#[test]
fn websocket_candle_preserves_literal_zero_volume() {
    let route = &route()[0];
    let cache = Arc::new(RwLock::new(MarketCache::default()));

    insert_candle(
        &cache,
        route,
        FixedValue::parse("100").unwrap(),
        FixedValue::ZERO,
        42,
    );

    let cache = cache.read().unwrap();
    assert_eq!(cache.candles["ETH/USDT"].len(), 1);
    assert!(cache.candles["ETH/USDT"][0].volume.is_zero());
}

#[test]
fn coen_provider_symbols_are_valid_websocket_source_markets() {
    let binance = ExchangeKind::Binance
        .pair_route("COEN", "USDT")
        .expect("COEN/USDT is an explicit provider market");
    assert_eq!(binance.key, "COEN/USDT");
    assert_eq!(binance.provider_symbol, "COENUSDT");

    let coinbase = ExchangeKind::Coinbase
        .pair_route("COEN", "USDC")
        .expect("COEN/USDC is an explicit provider market");
    assert_eq!(coinbase.provider_symbol, "COEN-USDC");
}

#[test]
fn exchange_decoders_populate_fixed_point_ticker_and_candle_cache() {
    let cases = [
        (
            ExchangeKind::Binance,
            br#"{"s":"ETHUSDT","c":"2500.125","v":"42.5"}"#.as_slice(),
            br#"{"s":"ETHUSDT","k":{"c":"2501.25","v":"3.5","T":1700000000000}}"#.as_slice(),
        ),
        (
            ExchangeKind::Kraken,
            br#"[1,{"c":["2500.125","1"],"v":["1","42.5"]},"ticker","ETH/USDT"]"#.as_slice(),
            br#"[1,["1699999940.0","1700000000.0","1","2","1","2501.25","2","3.5","1"],"ohlc-1","ETH/USDT"]"#.as_slice(),
        ),
        (
            ExchangeKind::Okx,
            br#"{"arg":{"channel":"tickers","instId":"ETHUSDT"},"data":[{"last":"2500.125","vol24h":"42.5"}]}"#.as_slice(),
            br#"{"arg":{"channel":"candle1m","instId":"ETHUSDT"},"data":[["1700000000000","1","2","1","2501.25","3.5"]]}"#.as_slice(),
        ),
        (
            ExchangeKind::Gate,
            br#"{"channel":"spot.tickers","event":"update","result":{"currency_pair":"ETHUSDT","last":"2500.125","base_volume":"42.5"}}"#.as_slice(),
            br#"{"channel":"spot.candlesticks","event":"update","result":{"n":"1m_ETHUSDT","t":"1700000000","c":"2501.25","v":"3.5","a":"8754.375"}}"#.as_slice(),
        ),
        (
            ExchangeKind::Huobi,
            br#"{"ch":"market.ethusdt.ticker","tick":{"lastPrice":"2500.125","vol":"42.5"}}"#.as_slice(),
            br#"{"ch":"market.ethusdt.kline.1min","tick":{"close":"2501.25","vol":"3.5","id":1700000000}}"#.as_slice(),
        ),
    ];

    for (kind, ticker, candle) in cases {
        let cache = Arc::new(RwLock::new(MarketCache::default()));
        process_payload(kind, ticker, &route(), &cache).unwrap();
        process_payload(kind, candle, &route(), &cache).unwrap();
        let cache = cache.read().unwrap();
        assert_eq!(
            cache.tickers["ETH/USDT"].price,
            FixedValue::parse("2500.125").unwrap(),
            "{kind:?} ticker"
        );
        assert_eq!(
            cache.candles["ETH/USDT"][0].price,
            FixedValue::parse("2501.25").unwrap(),
            "{kind:?} candle"
        );
    }
}

#[test]
fn mexc_protobuf_decoder_populates_fixed_point_ticker_and_candle_cache() {
    let cache = Arc::new(RwLock::new(MarketCache::default()));
    // Golden wire bytes assembled from MEXC's published .proto tags, not
    // encoded by the Rust types under test. Wrapper body tags are 309 for
    // miniTicker and 308 for kline; wrapper symbol is tag 3.
    let ticker = b"\xaa\x13\x19\x0a\x07ETHUSDT\x12\x082500.125\x42\x0442.5\x1a\x07ETHUSDT";
    let candle = b"\xa2\x13\x14\x0a\x04Min1\x22\x072501.25\x3a\x033.5\x1a\x07ETHUSDT";

    process_payload(ExchangeKind::Mexc, ticker, &route(), &cache).unwrap();
    process_payload(ExchangeKind::Mexc, candle, &route(), &cache).unwrap();

    let cache = cache.read().unwrap();
    assert_eq!(
        cache.tickers["ETH/USDT"].price,
        FixedValue::parse("2500.125").unwrap()
    );
    assert_eq!(
        cache.candles["ETH/USDT"][0].price,
        FixedValue::parse("2501.25").unwrap()
    );
}

#[test]
fn coinbase_ticker_decoder_does_not_invent_candles_from_trades() {
    let cache = Arc::new(RwLock::new(MarketCache::default()));
    process_payload(
        ExchangeKind::Coinbase,
        br#"{"type":"ticker","product_id":"ETHUSDT","price":"2500.125","volume_24h":"42.5"}"#,
        &route(),
        &cache,
    )
    .unwrap();
    process_payload(
        ExchangeKind::Coinbase,
        br#"{"type":"match","product_id":"ETHUSDT","price":"2501.25","size":"3.5"}"#,
        &route(),
        &cache,
    )
    .unwrap();

    let cache = cache.read().unwrap();
    assert_eq!(
        cache.tickers["ETH/USDT"].price,
        FixedValue::parse("2500.125").unwrap()
    );
    assert!(cache.candles.is_empty());
}

#[test]
fn huobi_application_ping_returns_matching_pong() {
    let cache = Arc::new(RwLock::new(MarketCache::default()));
    let response = process_payload(
        ExchangeKind::Huobi,
        br#"{"ping":1492420473027}"#,
        &route(),
        &cache,
    )
    .unwrap();
    assert_eq!(response.as_deref(), Some(r#"{"pong":1492420473027}"#));
}

#[test]
fn huobi_gzip_decoder_rejects_oversized_payloads() {
    use std::io::Write as _;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder
        .write_all(&vec![0; MAX_DECOMPRESSED_MESSAGE_BYTES + 1])
        .unwrap();
    let compressed = encoder.finish().unwrap();
    assert!(decompress_gzip(&compressed).is_err());
}

#[test]
fn reconnect_backoff_resets_only_after_data_or_a_stable_session() {
    let current = Duration::from_secs(8);

    assert_eq!(
        reconnect_delay_after_session(current, false, Duration::from_secs(1)),
        current,
        "a successful handshake followed by an immediate close must retain backoff"
    );
    assert_eq!(
        reconnect_delay_after_session(current, true, Duration::from_secs(1)),
        Duration::from_secs(1)
    );
    assert_eq!(
        reconnect_delay_after_session(current, false, STABLE_SESSION_AFTER),
        Duration::from_secs(1)
    );
}

#[tokio::test]
async fn reconnects_resubscribes_answers_ping_and_replaces_cached_ticker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let subscriptions = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&subscriptions);
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut finish_rx = Some(finish_rx);
        for (connection, price) in ["2500.125", "2501.25"].into_iter().enumerate() {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let subscription = socket.next().await.unwrap().unwrap();
            assert!(subscription.is_text());
            observed.fetch_add(1, Ordering::SeqCst);
            socket
                .send(Message::Text(
                    format!(r#"{{"s":"ETHUSDT","c":"{price}","v":"42.5"}}"#).into(),
                ))
                .await
                .unwrap();
            socket.send(Message::Ping(vec![7].into())).await.unwrap();
            loop {
                match socket.next().await.unwrap().unwrap() {
                    Message::Pong(payload) if payload.as_ref() == [7] => break,
                    _ => {}
                }
            }
            if connection == 0 {
                socket.close(None).await.unwrap();
            } else {
                finish_rx.take().unwrap().await.unwrap();
                socket.close(None).await.unwrap();
            }
        }
    });

    let endpoint = ApiProviderConfig {
        name: "binance".to_owned(),
        rest: String::new(),
        websocket: format!("ws://{address}"),
        api_key: String::new(),
    };
    let pairs = vec![("ETH".to_owned(), "USDT".to_owned())];
    let provider = StreamingProvider::new(
        ExchangeKind::Binance,
        Some(&endpoint),
        &pairs,
        Box::new(EmptyFallback),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let values = provider.cached_tickers(&pairs);
        if values
            .get("ETH/USDT")
            .is_some_and(|ticker| ticker.price == FixedValue::parse("2501.25").unwrap())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "second streamed ticker was not cached"
        );
        sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(subscriptions.load(Ordering::SeqCst), 2);
    finish_tx.send(()).unwrap();
    server.await.unwrap();
}
