//! Read-only view precompile for the Intex module.
//!
//! Writes stay Rust-to-Rust (IntexFactory). This surface only exposes reads so
//! off-chain consumers can observe the canonical series identity + lifecycle.
//! Every method is a view. `reject_value` rejects any `msg.value` before a read.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::errors::IntexError;
use crate::schema::{CallStamp, IntexContract, SeriesId, SeriesRecord};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Generated ABI methods follow the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IIntex.sol"
    );
}
pub use abi::IIntex;

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
            getIntexSeries(c) => view(c, |c| {
                let series_id = SeriesId::from(c.seriesId);
                let record = registry.load_series(series_id)?;
                let settled = registry.settled_units.read(&series_id)?;
                let exercised = registry.exercised_units.read(&series_id)?;
                let gem_factory = registry.gem_factory_units.read(&series_id)?;
                let stamp = registry.call_stamp(&record)?;
                let now = storage.now_secs()?;
                to_abi_data(&record, [settled, exercised, gem_factory], stamp, now)
            }),
            seriesUnitCounts(c) => view(c, |c| {
                let counts = crate::api::unit_counts(&storage, SeriesId::from(c.seriesId))?;
                Ok(IIntex::UnitCounts {
                    issuedUnits: counts.issued,
                    activeUnits: counts.active,
                    settledUnits: counts.settled,
                    exercisedUnits: counts.exercised,
                    gemFactoryUnits: counts.gem_factory,
                    forfeitedUnits: counts.forfeited,
                })
            }),
            seriesExists(c) => view(c, |c| registry.series_exists(SeriesId::from(c.seriesId))),
            totalSeries(_) => {
                metadata::<IIntex::totalSeriesCall>(|| Ok(u64::from(registry.read_total_series()?)))
            }
            seriesAt(c) => view(c, |c| {
                let index =
                    u32::try_from(c.index).map_err(|_| IntexError::SeriesIndexOutOfRange)?;
                Ok(registry.read_series_id_at(index)?.into())
            }),
        }
    })
}

fn to_abi_data(
    r: &SeriesRecord,
    [settled, exercised, gem_factory]: [u32; 3],
    stamp: CallStamp,
    now: u64,
) -> Result<IIntex::SeriesData> {
    Ok(IIntex::SeriesData {
        seriesId: r.series_id.into(),
        promisLoadMinor: r.promis_load_minor,
        entryPriceMinor: r.entry_price_minor,
        issuedUnits: r.issued_units,
        callWindow: r.call_window_seconds,
        callThreshold: r.call_threshold_seconds,
        callPriceMinor: r.call_price_minor,
        state: stamp.state(now) as u8,
        issuedAt: r.issued_at,
        calledAt: stamp.called_at,
        callNoticePeriod: r.call_notice_period_seconds,
        issuanceCurrency: r.issuance_currency,
        referenceCurrency: r.reference_currency,
        worldwideDay: r.worldwide_day.into(),
        settledUnits: settled,
        exercisedUnits: exercised,
        gemFactoryUnits: gem_factory,
        settlementDeadline: stamp.deadline,
        groupKey: r.group_key(),
    })
}
