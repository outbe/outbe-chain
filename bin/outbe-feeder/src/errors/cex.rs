//! Error types for centralized exchange WebSocket and configuration handling.

use std::time::Duration;
use thiserror::Error;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

/// Errors that occur during CEX market validation or WebSocket streaming.
#[derive(Debug, Error)]
pub(crate) enum CexError {
    /// An exchange market quote uses a numeric ISO 4217 code instead of an exchange symbol.
    #[error("exchange '{exchange}' market quote '{quote}' is an ISO code; write the exchange's market, such as USDT, USDC or USD")]
    IsoMarketQuote {
        /// The exchange name.
        exchange: String,
        /// The invalid ISO quote code.
        quote: String,
    },
    /// The WebSocket stream ended without a close frame.
    #[error("websocket stream ended")]
    StreamEnded,
    /// The remote exchange closed the WebSocket connection.
    #[error("remote closed websocket: {0:?}")]
    RemoteClosed(Option<CloseFrame>),
    /// The WebSocket connection received no data or pong within the timeout window.
    #[error("websocket received no data or pong for {0:?}")]
    StaleConnection(Duration),
    /// A Kraken ticker message lacks a close price.
    #[error("kraken ticker has no close")]
    KrakenMissingClose,
    /// A Kraken ticker message lacks a 24-hour volume value.
    #[error("kraken ticker has no 24h volume")]
    KrakenMissingVolume,
    /// A Kraken OHLC message payload is not a JSON array.
    #[error("kraken candle is not an array")]
    KrakenCandleNotArray,
    /// A MEXC mini-ticker message has an invalid price string.
    #[error("invalid MEXC mini-ticker price")]
    InvalidMexcTickerPrice,
    /// A MEXC mini-ticker message has an invalid quantity string.
    #[error("invalid MEXC mini-ticker quantity")]
    InvalidMexcTickerQuantity,
    /// A MEXC kline message has an invalid closing price string.
    #[error("invalid MEXC candle close")]
    InvalidMexcCandleClose,
    /// A MEXC kline message has an invalid volume string.
    #[error("invalid MEXC candle volume")]
    InvalidMexcCandleVolume,
    /// A JSON object lacks a required decimal field.
    #[error("missing decimal field `{0}`")]
    MissingDecimalField(String),
    /// A JSON array lacks a required decimal element at the given index.
    #[error("missing decimal index {0}")]
    MissingDecimalIndex(usize),
    /// A JSON value is not a valid fixed-point decimal lexeme.
    #[error("invalid fixed-point decimal `{0}`")]
    InvalidFixedPointDecimal(String),
    /// A gzip-compressed WebSocket frame exceeds the maximum decompressed size.
    #[error("decompressed websocket message exceeds size limit")]
    DecompressedMessageTooLarge,
}
