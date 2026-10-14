//! MEXC price provider.

use async_trait::async_trait;
use eyre::Result;
use std::collections::HashMap;

use super::rest::ticker_24h;
use crate::provider::{Provider, TickerPrice};

/// Maps a (base, quote) pair to a MEXC symbol.
/// Returns `None` for pairs MEXC doesn't support.
fn map_symbol(base: &str, quote: &str) -> Option<String> {
    if base.starts_with("COEN") || base.starts_with("0x") {
        return None;
    }
    let q = quote;
    Some(format!("{base}{q}"))
}

/// MEXC price provider.
pub struct MexcProvider {
    client: reqwest::Client,
}

impl MexcProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl Provider for MexcProvider {
    fn name(&self) -> &str {
        "mexc"
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        Ok(ticker_24h(
            &self.client,
            "mexc",
            "https://api.mexc.com/api/v3/ticker/24hr",
            pairs,
            map_symbol,
        )
        .await)
    }
}
