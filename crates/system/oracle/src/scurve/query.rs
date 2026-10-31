//! Reads of the per-pair S-curve ring.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use outbe_primitives::time::{WorldwideDay, SECONDS_PER_DAY};

use super::compute_scurve_value;
use super::store::SCURVE_RING_SLOTS;
use crate::schema::OracleContract;

/// `(bases, quotes, peak_days, peak_prices)` - every stored S-curve.
type ScurveTable = (Vec<Address>, Vec<Address>, Vec<u64>, Vec<U256>);

/// The stored curves of `pair` as `(start WWD, peak price)`, oldest first.
fn curves(oracle: &OracleContract, pair: AddressPair) -> Result<Vec<(WorldwideDay, U256)>> {
    let len = oracle.scurve_len.read(&pair)?;
    let starts = oracle.scurve_peak_wwd.get_nested(&pair);
    let prices = oracle.scurve_peak_price.get_nested(&pair);
    (len.saturating_sub(SCURVE_RING_SLOTS)..len)
        .map(|n| {
            let slot = n % SCURVE_RING_SLOTS;
            Ok((starts.read(&slot)?, prices.read(&slot)?))
        })
        .collect()
}

/// The curve of `pair` in force at `wwd`: the newest curve that starts at `wwd`
/// or earlier.
///
/// Curve starts grow with the curve number, so the scan goes from the newest
/// curve to the oldest and stops at the first start at or before `wwd`. A
/// recent `wwd` reads one or two slots, not the whole ring. Only the found
/// curve reads its price.
pub(super) fn curve_at(
    oracle: &OracleContract,
    pair: AddressPair,
    wwd: WorldwideDay,
) -> Result<Option<(WorldwideDay, U256)>> {
    let len = oracle.scurve_len.read(&pair)?;
    let starts = oracle.scurve_peak_wwd.get_nested(&pair);
    for n in (len.saturating_sub(SCURVE_RING_SLOTS)..len).rev() {
        let slot = n % SCURVE_RING_SLOTS;
        let start = starts.read(&slot)?;
        if start <= wwd {
            let price = oracle.scurve_peak_price.get_nested(&pair).read(&slot)?;
            return Ok(Some((start, price)));
        }
    }
    Ok(None)
}

/// The value at `wwd` of the curve that starts at `start` with `peak_price`.
/// `wwd` must not be before `start`.
pub(super) fn curve_value(
    start: WorldwideDay,
    peak_price: U256,
    wwd: WorldwideDay,
) -> Result<U256> {
    let wwd_ts = wwd
        .to_timestamp_utc()
        .map_err(crate::errors::date_step_error)?;
    let start_ts = start
        .to_timestamp_utc()
        .map_err(crate::errors::date_step_error)?;
    let days = (wwd_ts - start_ts) / SECONDS_PER_DAY;
    Ok(compute_scurve_value(peak_price, days as usize))
}

/// The S-curve value of `pair` at `wwd`, or 0 when no curve starts at `wwd` or
/// earlier.
pub fn scurve_value(oracle: &OracleContract, pair: AddressPair, wwd: WorldwideDay) -> Result<U256> {
    match curve_at(oracle, pair, wwd)? {
        Some((start, price)) => curve_value(start, price, wwd),
        None => Ok(U256::ZERO),
    }
}

/// The curves of `pair` that start at `wwd` or earlier, oldest first, as
/// `(start WWDs, peak prices, values at wwd)`.
pub fn get_scurve_entries(
    oracle: &OracleContract,
    pair: AddressPair,
    wwd: WorldwideDay,
) -> Result<(Vec<u64>, Vec<U256>, Vec<U256>)> {
    let mut starts = Vec::new();
    let mut prices = Vec::new();
    let mut values = Vec::new();
    for (start, price) in curves(oracle, pair)? {
        if start <= wwd {
            starts.push(start.into());
            prices.push(price);
            values.push(curve_value(start, price, wwd)?);
        }
    }
    Ok((starts, prices, values))
}

/// Every stored curve of every registered pair, in registry order.
pub fn get_all_scurve_data(oracle: &OracleContract) -> Result<ScurveTable> {
    let mut table = ScurveTable::default();
    for index in 1..=oracle.pair_count.read()? {
        let pair = oracle.pair_at(index)?;
        for (start, price) in curves(oracle, pair)? {
            table.0.push(pair.address1());
            table.1.push(pair.address2());
            table.2.push(start.into());
            table.3.push(price);
        }
    }
    Ok(table)
}

/// Every stored curve of `pair`, oldest first, as `(start WWDs, peak prices)`.
pub fn get_all_scurve_data_for_pair(
    oracle: &OracleContract,
    pair: AddressPair,
) -> Result<(Vec<u64>, Vec<U256>)> {
    Ok(curves(oracle, pair)?
        .into_iter()
        .map(|(start, price)| (u64::from(start), price))
        .unzip())
}
