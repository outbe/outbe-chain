//! Read-only view precompile for the IntexRegistry module.
//!
//! Writes stay Rust-to-Rust (IntexFactory); this surface only exposes reads so
//! off-chain consumers can observe the canonical series identity + lifecycle.
//! Every method is a view; `reject_value` rejects any `msg.value` before a read.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::schema::{IntexRegistryContract, SeriesRecord};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IIntexRegistry.sol"
);

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(
        data,
        IIntexRegistry::IIntexRegistryCalls::abi_decode,
        |call| {
            let registry = IntexRegistryContract::new(storage.clone());
            use IIntexRegistry::IIntexRegistryCalls::*;
            match call {
                seriesData(c) => view(c, |c| {
                    let record = registry.load_series(c.seriesId)?;
                    Ok(to_abi_data(&record))
                }),
                seriesExists(c) => view(c, |c| registry.series_exists(c.seriesId)),
                totalSeries(_) => {
                    metadata::<IIntexRegistry::totalSeriesCall>(|| registry.read_total_series())
                }
                seriesAt(c) => view(c, |c| registry.read_series_id_at(c.index)),
            }
        },
    )
}

fn to_abi_data(r: &SeriesRecord) -> IIntexRegistry::SeriesData {
    IIntexRegistry::SeriesData {
        seriesId: r.series_id,
        intexSize: r.intex_size,
        intexStrikePrice: r.intex_strike_price,
        coenPriceFloor: r.coen_price_floor,
        issuedIntexCount: r.issued_intex_count,
        callWindowDays: r.call_window_days,
        callThresholdDays: r.call_threshold_days,
        coenPriceCallTrigger: r.coen_price_call_trigger,
        state: r.state,
        issuedAt: r.issued_at,
        calledAt: r.called_at,
        intexCallPeriod: r.intex_call_period,
    }
}
