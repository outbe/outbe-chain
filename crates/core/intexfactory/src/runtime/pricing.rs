use super::*;
use outbe_vaultrouter::settlement::{Currencies, Obligation};

pub(crate) fn currencies(series: &outbe_intex::SeriesRecord) -> Currencies {
    Currencies {
        issuance: series.issuance_currency,
        reference: series.reference_currency,
    }
}

/// `units` of a series: its entry price times its Promis load per unit.
pub(crate) fn obligation(series: &outbe_intex::SeriesRecord, units: U256) -> Obligation {
    Obligation {
        entry_price_minor: series.entry_price_minor,
        load_minor: series.promis_load_minor,
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
    payment_decimals: u8,
) -> Result<U256> {
    let obligation = Obligation {
        entry_price_minor: product,
        load_minor: U256::ONE,
        units,
        cost_rate_pct: 100,
    };
    outbe_common::settlement::obligation_units(&obligation, rate, payment_decimals)
        .map_err(|e| settlement::SettlementError::Rounding(e).into())
}

/// Whether the series has qualified. It is derived from finalized daily VWAPs and never stored.
pub fn is_qualified(
    storage: &StorageHandle<'_>,
    series: &outbe_intex::SeriesRecord,
) -> Result<bool> {
    outbe_intex::api::is_qualified(storage, series)
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
        &obligation(&series, units),
    )?;
    Ok((
        quote.settlement_currency,
        quote.payment_minor,
        quote.snapshot_id(),
    ))
}
