use super::*;
use outbe_vaultrouter::settlement::{Currencies, SettlementCost};

pub(crate) fn currencies(series: &outbe_intex::SeriesRecord) -> Currencies {
    Currencies {
        issuance: series.issuance_currency,
        reference: series.reference_currency,
    }
}

/// `units` of a series: its entry price times its Promis load per unit.
pub(crate) fn settlement_cost(series: &outbe_intex::SeriesRecord, units: U256) -> SettlementCost {
    series_cost(series.entry_price_minor, series.promis_load_minor, units)
}

/// `units` at an entry price and Promis load per unit, at cost.
fn series_cost(entry_price_minor: U256, load_minor: U256, units: U256) -> SettlementCost {
    SettlementCost {
        entry_price_minor,
        load_minor,
        units,
        cost_rate_pct: 100,
    }
}

/// Cost of `units` at a per-unit `product` of entry price and load.
#[cfg(test)]
pub(crate) fn settlement_units(
    product: U256,
    units: U256,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256> {
    let cost = series_cost(product, U256::ONE, units);
    outbe_common::settlement::settlement_cost_units(&cost, rate, asset_decimals)
        .map_err(|e| settlement::SettlementError::Rounding(e).into())
}

/// What settling `units` of `series_id` with `asset` costs, which
/// of the series' two currencies that asset settles on, and the VWAP snapshot an
/// issuance-currency payment must name (zero on the reference rail). Priced exactly
/// as `settleIntex` charges it. Rejects an asset the series does not accept.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    asset: Address,
    units: U256,
) -> Result<(u16, U256, U256)> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    let quote = settlement::quote(
        storage,
        asset,
        currencies(&series),
        &settlement_cost(&series, units),
    )?;
    Ok((
        quote.settlement_currency,
        quote.payment_minor,
        quote.snapshot_id(),
    ))
}
