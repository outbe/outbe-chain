//! The gas oracle of the Outbe IGP: `getExchangeRateAndGasPrice`.
//!
//! The Oracle keeps the gas price of each remote chain in COEN-wei, so the
//! exchange rate is 1:1. The view adds the markup of `gas_markup_bps`.

use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::schema::HyperlaneControllerContract;

/// `TOKEN_EXCHANGE_RATE_SCALE` of the Hyperlane IGP. As an exchange rate it
/// means 1:1: the Oracle gas price is already in COEN.
pub const TOKEN_EXCHANGE_RATE_SCALE: u128 = 10_000_000_000;
/// Markup that `initialize` sets, in basis points: +20%.
pub const GAS_MARKUP_BPS: u32 = 2_000;

impl HyperlaneControllerContract<'_> {
    /// `(tokenExchangeRate, gasPrice)` for `domain`: 1:1 and the Oracle gas
    /// price plus the markup. Reverts when the Oracle has no gas price for
    /// `domain`, so a quote is never zero, or when the price does not fit in
    /// 128 bits.
    pub fn gas_quote(&self, domain: u32) -> Result<(u128, u128)> {
        let Some((price, _)) = outbe_oracle::api::gas_price(self.storage.clone(), domain)? else {
            return Err(HyperlaneControllerError::NoGasPrice { domain }.into());
        };
        let markup = U256::from(10_000) + U256::from(self.gas_markup_bps.read()?);
        let quoted = price
            .checked_mul(markup)
            .map(|value| value / U256::from(10_000))
            .and_then(|value| u128::try_from(value).ok())
            .ok_or(HyperlaneControllerError::GasPriceOverflow { domain })?;
        Ok((TOKEN_EXCHANGE_RATE_SCALE, quoted))
    }
}
