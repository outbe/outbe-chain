//! Reads of the S-curve of a pair.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_system_common::address_pair::AddressPair;
use outbe_system_common::time::SECONDS_PER_DAY;

use super::compute_scurve_value;
use crate::schema::OracleContract;

/// `(bases, quotes, start_hours, peak_prices)` - the curve of each pair that has one.
type ScurveTable = (Vec<Address>, Vec<Address>, Vec<u64>, Vec<U256>);

/// The curve of `pair` as `(start hour, peak price)`, or `None` when the pair
/// has no curve.
pub fn curve(oracle: &OracleContract, pair: AddressPair) -> Result<Option<(u64, U256)>> {
    let start_hour = oracle.scurve_start_hour.read(&pair)?;
    if start_hour == 0 {
        return Ok(None);
    }
    Ok(Some((start_hour, oracle.scurve_peak_price.read(&pair)?)))
}

/// The value at `timestamp` of the curve that starts at `start_hour` with
/// `peak_price`. The curve counts whole periods of 24 h from its start hour.
/// `timestamp` must not be before `start_hour`.
pub(super) fn curve_value(start_hour: u64, peak_price: U256, timestamp: u64) -> U256 {
    let days = (timestamp - start_hour) / SECONDS_PER_DAY;
    compute_scurve_value(peak_price, days as usize)
}

/// The S-curve value of `pair` at `timestamp`, or 0 when the pair has no curve
/// or its curve starts after `timestamp`.
pub fn scurve_value(oracle: &OracleContract, pair: AddressPair, timestamp: u64) -> Result<U256> {
    Ok(match curve(oracle, pair)? {
        Some((start_hour, price)) if start_hour <= timestamp => {
            curve_value(start_hour, price, timestamp)
        }
        _ => U256::ZERO,
    })
}

/// The curve of every registered pair that has one, in registry order.
pub fn get_all_scurve_data(oracle: &OracleContract) -> Result<ScurveTable> {
    let mut table = ScurveTable::default();
    for index in 1..=oracle.pair_count.read()? {
        let pair = oracle.pair_at(index)?;
        if let Some((start_hour, price)) = curve(oracle, pair)? {
            table.0.push(pair.address1());
            table.1.push(pair.address2());
            table.2.push(start_hour);
            table.3.push(price);
        }
    }
    Ok(table)
}
