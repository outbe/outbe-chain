//! Exchange-specific subscription messages and market-data decoding for the
//! WebSocket transport. Every exchange maps to the same cache writes.

use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

use eyre::{eyre, Result, WrapErr};
use flate2::read::GzDecoder;
use prost::Message as ProstMessage;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::{
    ExchangeKind, PairRoute, SharedCache, MAX_CANDLES_PER_PAIR, MAX_DECOMPRESSED_MESSAGE_BYTES,
};
use crate::fixed::FixedValue;
use crate::provider::{checked_candle, checked_ticker, VolumeInput};

pub(super) fn subscription_messages(
    kind: ExchangeKind,
    routes: &[PairRoute],
) -> Result<Vec<String>> {
    let symbols = routes
        .iter()
        .map(|route| route.provider_symbol.clone())
        .collect::<Vec<_>>();
    let messages = match kind {
        ExchangeKind::Binance => vec![json!({
            "method": "SUBSCRIBE",
            "params": symbols.iter().flat_map(|symbol| {
                let symbol = symbol.to_ascii_lowercase();
                [format!("{symbol}@ticker"), format!("{symbol}@kline_1m")]
            }).collect::<Vec<_>>(),
            "id": 1
        })],
        ExchangeKind::Kraken => vec![
            json!({"event": "subscribe", "pair": symbols, "subscription": {"name": "ticker"}}),
            json!({"event": "subscribe", "pair": symbols, "subscription": {"name": "ohlc"}}),
        ],
        ExchangeKind::Okx => vec![json!({
            "op": "subscribe",
            "args": symbols.iter().flat_map(|symbol| [
                json!({"channel": "tickers", "instId": symbol}),
                json!({"channel": "candle1m", "instId": symbol}),
            ]).collect::<Vec<_>>()
        })],
        ExchangeKind::Gate => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let now = i64::try_from(now).unwrap_or(i64::MAX);
            let mut values = vec![json!({
                "time": now,
                "channel": "spot.tickers",
                "event": "subscribe",
                "payload": symbols,
                "id": 1
            })];
            values.extend(symbols.iter().map(|symbol| {
                json!({
                    "time": now,
                    "channel": "spot.candlesticks",
                    "event": "subscribe",
                    "payload": ["1m", symbol],
                    "id": 2
                })
            }));
            values
        }
        ExchangeKind::Huobi => symbols
            .iter()
            .flat_map(|symbol| {
                [
                    json!({"sub": format!("market.{symbol}.ticker")}),
                    json!({"sub": format!("market.{symbol}.kline.1min")}),
                ]
            })
            .collect(),
        ExchangeKind::Mexc => {
            vec![json!({
                "method": "SUBSCRIPTION",
                "params": symbols.iter().flat_map(|symbol| [
                    format!("spot@public.miniTicker.v3.api.pb@{symbol}@UTC+8"),
                    format!("spot@public.kline.v3.api.pb@{symbol}@Min1"),
                ]).collect::<Vec<_>>()
            })]
        }
        ExchangeKind::Coinbase => vec![json!({
            "type": "subscribe",
            "product_ids": symbols,
            "channels": ["ticker"]
        })],
    };
    messages
        .into_iter()
        .map(|message| serde_json::to_string(&message).map_err(Into::into))
        .collect()
}

pub(super) fn process_payload(
    kind: ExchangeKind,
    payload: &[u8],
    routes: &[PairRoute],
    cache: &SharedCache,
) -> Result<Option<String>> {
    if kind == ExchangeKind::Mexc {
        // Subscription acknowledgements and PONG responses are JSON, while
        // market data is binary Protocol Buffers.
        if payload
            .first()
            .is_some_and(|byte| matches!(byte, b'{' | b'['))
        {
            let _: Value =
                serde_json::from_slice(payload).wrap_err("decode MEXC websocket control JSON")?;
            return Ok(None);
        }
        return parse_mexc(payload, routes, cache);
    }
    let value: Value =
        serde_json::from_slice(payload).wrap_err("decode exchange websocket JSON")?;
    match kind {
        ExchangeKind::Binance => parse_binance(&value, routes, cache),
        ExchangeKind::Kraken => parse_kraken(&value, routes, cache),
        ExchangeKind::Okx => parse_okx(&value, routes, cache),
        ExchangeKind::Gate => parse_gate(&value, routes, cache),
        ExchangeKind::Huobi => parse_huobi(&value, routes, cache),
        ExchangeKind::Mexc => unreachable!("MEXC payload handled before JSON decoding"),
        ExchangeKind::Coinbase => parse_coinbase(&value, routes, cache),
    }
}

fn parse_binance(
    value: &Value,
    routes: &[PairRoute],
    cache: &SharedCache,
) -> Result<Option<String>> {
    if let Some(kline) = value.get("k").and_then(Value::as_object) {
        let symbol = string_field(value, "s").or_else(|| string_map_field(kline, "s"));
        if let Some(route) = symbol.and_then(|symbol| find_route(routes, symbol)) {
            insert_candle(
                cache,
                route,
                decimal_map_field(kline, "c")?,
                decimal_map_field(kline, "v")?,
                integer_map_field(kline, "T")
                    .or_else(|| integer_map_field(kline, "t"))
                    .unwrap_or(0),
            );
        }
        return Ok(None);
    }
    if let Some(route) = string_field(value, "s").and_then(|symbol| find_route(routes, symbol)) {
        insert_ticker(
            cache,
            route,
            decimal_field(value, "c").or_else(|_| decimal_field(value, "lastPrice"))?,
            decimal_field(value, "v").or_else(|_| decimal_field(value, "volume"))?,
        );
    }
    Ok(None)
}

fn parse_kraken(
    value: &Value,
    routes: &[PairRoute],
    cache: &SharedCache,
) -> Result<Option<String>> {
    let Some(message) = value.as_array() else {
        return Ok(None);
    };
    if message.len() != 4 {
        return Ok(None);
    }
    let Some(channel) = message[2].as_str() else {
        return Ok(None);
    };
    let Some(symbol) = message[3].as_str() else {
        return Ok(None);
    };
    let Some(route) = find_route(routes, symbol) else {
        return Ok(None);
    };
    if channel == "ticker" {
        let body = &message[1];
        let price = body["c"]
            .as_array()
            .and_then(|values| values.first())
            .ok_or_else(|| eyre!("kraken ticker has no close"))?;
        let volume = body["v"]
            .as_array()
            .and_then(|values| values.get(1))
            .ok_or_else(|| eyre!("kraken ticker has no 24h volume"))?;
        insert_ticker(cache, route, decimal_value(price)?, decimal_value(volume)?);
    } else if channel.starts_with("ohlc-") {
        let values = message[1]
            .as_array()
            .ok_or_else(|| eyre!("kraken candle is not an array"))?;
        insert_candle(
            cache,
            route,
            decimal_index(values, 5)?,
            decimal_index(values, 7)?,
            integer_index(values, 1).unwrap_or(0),
        );
    }
    Ok(None)
}

fn parse_okx(value: &Value, routes: &[PairRoute], cache: &SharedCache) -> Result<Option<String>> {
    let channel = string_field(&value["arg"], "channel");
    let symbol = string_field(&value["arg"], "instId");
    let Some(route) = symbol.and_then(|symbol| find_route(routes, symbol)) else {
        return Ok(None);
    };
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return Ok(None);
    };
    if channel == Some("tickers") {
        if let Some(ticker) = data.first() {
            insert_ticker(
                cache,
                route,
                decimal_field(ticker, "last")?,
                decimal_field(ticker, "vol24h")?,
            );
        }
    } else if channel.is_some_and(|channel| channel.starts_with("candle")) {
        if let Some(values) = data.first().and_then(Value::as_array) {
            insert_candle(
                cache,
                route,
                decimal_index(values, 4)?,
                decimal_index(values, 5)?,
                integer_index(values, 0).unwrap_or(0),
            );
        }
    }
    Ok(None)
}

fn parse_gate(value: &Value, routes: &[PairRoute], cache: &SharedCache) -> Result<Option<String>> {
    if string_field(value, "event") != Some("update") {
        return Ok(None);
    }
    let channel = string_field(value, "channel");
    let result = &value["result"];
    if channel == Some("spot.tickers") {
        let Some(route) =
            string_field(result, "currency_pair").and_then(|symbol| find_route(routes, symbol))
        else {
            return Ok(None);
        };
        insert_ticker(
            cache,
            route,
            decimal_field(result, "last")?,
            decimal_field(result, "base_volume")?,
        );
    } else if channel == Some("spot.candlesticks") {
        let Some(name) = string_field(result, "n") else {
            return Ok(None);
        };
        let symbol = name.split_once('_').map_or(name, |(_, pair)| pair);
        let Some(route) = find_route(routes, symbol) else {
            return Ok(None);
        };
        insert_candle(
            cache,
            route,
            decimal_field(result, "c")?,
            decimal_field(result, "v").or_else(|_| decimal_field(result, "a"))?,
            integer_field(result, "t").unwrap_or(0),
        );
    }
    Ok(None)
}

fn parse_huobi(value: &Value, routes: &[PairRoute], cache: &SharedCache) -> Result<Option<String>> {
    if let Some(ping) = value.get("ping").and_then(raw_integer_value) {
        return Ok(Some(json!({"pong": ping}).to_string()));
    }
    let Some(channel) = string_field(value, "ch") else {
        return Ok(None);
    };
    let Some(route) = routes.iter().find(|route| {
        normalize_symbol(channel).contains(&normalize_symbol(&route.provider_symbol))
    }) else {
        return Ok(None);
    };
    let tick = &value["tick"];
    if channel.contains(".kline.") {
        insert_candle(
            cache,
            route,
            decimal_field(tick, "close")?,
            decimal_field(tick, "vol")?,
            integer_field(tick, "id").unwrap_or(0),
        );
    } else if channel.ends_with(".ticker") {
        insert_ticker(
            cache,
            route,
            decimal_field(tick, "lastPrice").or_else(|_| decimal_field(tick, "close"))?,
            decimal_field(tick, "vol")?,
        );
    }
    Ok(None)
}

fn parse_mexc(payload: &[u8], routes: &[PairRoute], cache: &SharedCache) -> Result<Option<String>> {
    let message = MexcPushData::decode(payload).wrap_err("decode MEXC websocket protobuf")?;
    let Some(symbol) = message.symbol.as_deref() else {
        return Ok(None);
    };
    let Some(route) = find_route(routes, symbol) else {
        return Ok(None);
    };
    match message.body {
        Some(mexc_push_data::Body::MiniTicker(ticker)) => insert_ticker(
            cache,
            route,
            FixedValue::parse(&ticker.price)
                .ok_or_else(|| eyre!("invalid MEXC mini-ticker price"))?,
            FixedValue::parse(&ticker.quantity)
                .ok_or_else(|| eyre!("invalid MEXC mini-ticker quantity"))?,
        ),
        Some(mexc_push_data::Body::SpotKline(candle)) => insert_candle(
            cache,
            route,
            FixedValue::parse(&candle.closing_price)
                .ok_or_else(|| eyre!("invalid MEXC candle close"))?,
            FixedValue::parse(&candle.volume).ok_or_else(|| eyre!("invalid MEXC candle volume"))?,
            u64::try_from(candle.window_start).unwrap_or(0),
        ),
        None => {}
    }
    Ok(None)
}

#[derive(Clone, PartialEq, prost::Message)]
struct MexcPushData {
    #[prost(string, tag = "1")]
    channel: String,
    #[prost(oneof = "mexc_push_data::Body", tags = "308, 309")]
    body: Option<mexc_push_data::Body>,
    #[prost(string, optional, tag = "3")]
    symbol: Option<String>,
    #[prost(string, optional, tag = "4")]
    symbol_id: Option<String>,
    #[prost(int64, optional, tag = "5")]
    create_time: Option<i64>,
    #[prost(int64, optional, tag = "6")]
    send_time: Option<i64>,
}

mod mexc_push_data {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(super) enum Body {
        #[prost(message, tag = "308")]
        SpotKline(super::MexcSpotKline),
        #[prost(message, tag = "309")]
        MiniTicker(super::MexcMiniTicker),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
struct MexcMiniTicker {
    #[prost(string, tag = "1")]
    symbol: String,
    #[prost(string, tag = "2")]
    price: String,
    #[prost(string, tag = "3")]
    rate: String,
    #[prost(string, tag = "4")]
    zoned_rate: String,
    #[prost(string, tag = "5")]
    high: String,
    #[prost(string, tag = "6")]
    low: String,
    #[prost(string, tag = "7")]
    volume: String,
    #[prost(string, tag = "8")]
    quantity: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct MexcSpotKline {
    #[prost(string, tag = "1")]
    interval: String,
    #[prost(int64, tag = "2")]
    window_start: i64,
    #[prost(string, tag = "3")]
    opening_price: String,
    #[prost(string, tag = "4")]
    closing_price: String,
    #[prost(string, tag = "5")]
    highest_price: String,
    #[prost(string, tag = "6")]
    lowest_price: String,
    #[prost(string, tag = "7")]
    volume: String,
    #[prost(string, tag = "8")]
    amount: String,
    #[prost(int64, tag = "9")]
    window_end: i64,
}

fn parse_coinbase(
    value: &Value,
    routes: &[PairRoute],
    cache: &SharedCache,
) -> Result<Option<String>> {
    let Some(message_type) = string_field(value, "type") else {
        return Ok(None);
    };
    let Some(route) =
        string_field(value, "product_id").and_then(|symbol| find_route(routes, symbol))
    else {
        return Ok(None);
    };
    if message_type == "ticker" {
        insert_ticker(
            cache,
            route,
            decimal_field(value, "price")?,
            decimal_field(value, "volume_24h")?,
        );
    }
    Ok(None)
}

pub(super) fn insert_ticker(
    cache: &SharedCache,
    route: &PairRoute,
    price: FixedValue,
    volume: FixedValue,
) {
    let Some(ticker) = checked_ticker(
        "websocket",
        &route.key,
        Some(price),
        VolumeInput::Present(Some(volume)),
    ) else {
        return;
    };
    let mut cache = cache.write().unwrap_or_else(|error| error.into_inner());
    cache.tickers.insert(route.key.clone(), ticker);
    cache
        .ticker_updated
        .insert(route.key.clone(), Instant::now());
}

pub(super) fn clear_routes(cache: &SharedCache, routes: &[PairRoute]) {
    let mut cache = cache.write().unwrap_or_else(|error| error.into_inner());
    for route in routes {
        cache.tickers.remove(&route.key);
        cache.ticker_updated.remove(&route.key);
        cache.candles.remove(&route.key);
        cache.candle_updated.remove(&route.key);
    }
}

pub(super) fn insert_candle(
    cache: &SharedCache,
    route: &PairRoute,
    price: FixedValue,
    volume: FixedValue,
    timestamp: u64,
) {
    let Some(candle) = checked_candle(
        "websocket",
        &route.key,
        Some(price),
        VolumeInput::Present(Some(volume)),
        timestamp,
    ) else {
        return;
    };
    let mut cache = cache.write().unwrap_or_else(|error| error.into_inner());
    let candles = cache.candles.entry(route.key.clone()).or_default();
    if let Some(existing) = candles
        .iter_mut()
        .find(|candle| candle.timestamp == timestamp)
    {
        *existing = candle;
    } else {
        candles.push(candle);
    }
    candles.sort_unstable_by_key(|candle| candle.timestamp);
    if candles.len() > MAX_CANDLES_PER_PAIR {
        candles.drain(..candles.len() - MAX_CANDLES_PER_PAIR);
    }
    cache
        .candle_updated
        .insert(route.key.clone(), Instant::now());
}

fn find_route<'a>(routes: &'a [PairRoute], symbol: &str) -> Option<&'a PairRoute> {
    routes.iter().find(|route| route.matches(symbol))
}

pub(super) fn normalize_symbol(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect::<String>()
        .replace("XBT", "BTC")
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn string_map_field<'a>(value: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn decimal_field(value: &Value, key: &str) -> Result<FixedValue> {
    decimal_value(
        value
            .get(key)
            .ok_or_else(|| eyre!("missing decimal field `{key}`"))?,
    )
}

fn decimal_map_field(value: &serde_json::Map<String, Value>, key: &str) -> Result<FixedValue> {
    decimal_value(
        value
            .get(key)
            .ok_or_else(|| eyre!("missing decimal field `{key}`"))?,
    )
}

fn decimal_index(values: &[Value], index: usize) -> Result<FixedValue> {
    decimal_value(
        values
            .get(index)
            .ok_or_else(|| eyre!("missing decimal index {index}"))?,
    )
}

fn decimal_value(value: &Value) -> Result<FixedValue> {
    let lexeme = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    FixedValue::parse(&lexeme).ok_or_else(|| eyre!("invalid fixed-point decimal `{lexeme}`"))
}

fn integer_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(integer_value)
}

fn integer_map_field(value: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    value.get(key).and_then(integer_value)
}

fn integer_index(values: &[Value], index: usize) -> Option<u64> {
    values.get(index).and_then(integer_value)
}

fn integer_value(value: &Value) -> Option<u64> {
    raw_integer_value(value).map(normalize_timestamp)
}

fn raw_integer_value(value: &Value) -> Option<u64> {
    if let Some(value) = value.as_u64() {
        return Some(value);
    }
    let value = value.as_str()?;
    let whole = value.split_once('.').map_or(value, |(whole, _)| whole);
    whole.parse().ok()
}

fn normalize_timestamp(timestamp: u64) -> u64 {
    if timestamp >= 10_000_000_000 {
        timestamp / 1_000
    } else {
        timestamp
    }
}

pub(super) fn decompress_gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(bytes).take((MAX_DECOMPRESSED_MESSAGE_BYTES + 1) as u64);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    if output.len() > MAX_DECOMPRESSED_MESSAGE_BYTES {
        return Err(eyre!("decompressed websocket message exceeds size limit"));
    }
    Ok(output)
}
