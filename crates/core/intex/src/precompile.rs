//! Read-only view precompile for the Intex module.
//!
//! Writes stay Rust-to-Rust (IntexFactory); this surface only exposes reads so
//! off-chain consumers can observe the canonical series identity + lifecycle.
//! Every method is a view; `reject_value` rejects any `msg.value` before a read.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::schema::{IntexContract, SeriesRecord};
use crate::series_id::SeriesId;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IIntex.sol"
);

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IIntex::IIntexCalls::abi_decode, |call| {
        let registry = IntexContract::new(storage.clone());
        use IIntex::IIntexCalls::*;
        match call {
            seriesData(c) => view(c, |c| {
                let record = registry.load_series(SeriesId::from_raw(c.seriesId))?;
                to_abi_data(&record)
            }),
            seriesExists(c) => view(c, |c| {
                registry.series_exists(SeriesId::from_raw(c.seriesId))
            }),
            totalSeries(_) => metadata::<IIntex::totalSeriesCall>(|| registry.read_total_series()),
            seriesAt(c) => view(c, |c| Ok(registry.read_series_id_at(c.index)?.value())),
        }
    })
}

fn to_abi_data(r: &SeriesRecord) -> Result<IIntex::SeriesData> {
    Ok(IIntex::SeriesData {
        seriesId: r.series_id.value(),
        promisLoadMinor: r.promis_load_minor,
        entryPriceMinor: r.entry_price_minor,
        floorPriceMinor: r.floor_price_minor,
        issuedIntexCount: r.issued_intex_count,
        callWindow: r.call_window,
        callThreshold: r.call_threshold,
        callPriceMinor: r.call_price_minor,
        state: r.state,
        issuedAt: r.issued_at,
        calledAt: r.called_at,
        callNoticePeriod: r.call_notice_period,
        issuanceCurrency: r.issuance_currency,
        referenceCurrency: r.reference_currency,
        worldwideDay: r.worldwide_day,
        costAmountMinor: r.cost_amount_minor()?,
    })
}
