//! Cross pairs: `<base>/<ISO>` voted as `<base>/<cross>` ÷ `<ISO>/<cross>`.

use eyre::Result;

use super::{parse_oracle_asset, CurrencyPairConfig, FeederConfig};
use crate::errors::ConfigError;

impl FeederConfig {
    /// A cross pair is `<asset>/<ISO>` over another ISO code whose
    /// `<asset>/<cross>` pair is configured directly, not as a cross itself.
    pub(super) fn validate_cross(&self, pair: &CurrencyPairConfig) -> Result<()> {
        let Some(cross) = &pair.cross else {
            return Ok(());
        };
        let iso = |code: &str| code.parse::<u16>().is_ok_and(|c| (1..=999).contains(&c));
        if iso(&pair.base) || !iso(&pair.quote) || !iso(cross) {
            return Err(ConfigError::InvalidCrossPair {
                base: pair.base.clone(),
                quote: pair.quote.clone(),
                cross: cross.clone(),
            }
            .into());
        }
        if self.cross_leg(pair).is_none() {
            return Err(ConfigError::MissingCrossLeg {
                base: pair.base.clone(),
                quote: pair.quote.clone(),
                cross: cross.clone(),
            }
            .into());
        }
        Ok(())
    }

    /// The direct `<base>/<cross>` pair a cross pair divides.
    pub(crate) fn cross_leg(&self, pair: &CurrencyPairConfig) -> Option<&CurrencyPairConfig> {
        let cross = pair.cross.as_deref()?;
        let base = parse_oracle_asset(&pair.base).ok()?;
        self.currency_pairs.iter().find(|leg| {
            leg.cross.is_none()
                && leg.quote == cross
                && parse_oracle_asset(&leg.base).is_ok_and(|leg_base| leg_base == base)
        })
    }
}

impl CurrencyPairConfig {
    /// The asset the markets price: the quote currency for a cross pair.
    pub(crate) fn market_base(&self) -> &str {
        if self.cross.is_some() {
            &self.quote
        } else {
            &self.base
        }
    }

    /// The currency the markets price in: the cross currency for a cross pair.
    pub(crate) fn market_quote(&self) -> &str {
        self.cross.as_deref().unwrap_or(&self.quote)
    }

    /// Gives each market without a base or quote the pair's market base or
    /// market quote.
    pub(super) fn fill_market_defaults(&mut self) {
        let base = self.market_base().to_owned();
        let quote = self.market_quote().to_owned();
        for source in &mut self.sources {
            if source.base.is_empty() {
                source.base = base.clone();
            }
            if source.quote.is_empty() {
                source.quote = quote.clone();
            }
        }
    }
}
