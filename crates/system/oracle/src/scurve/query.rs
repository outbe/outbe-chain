//! Reads of the stored S-curve chains.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use super::{compute_scurve_value, truncate_to_day, DAY_SECONDS};
use crate::schema::OracleContract;

/// `(bases, quotes, peak_days, peak_prices)` - every active S-curve entry.
type ScurveTable = (Vec<Address>, Vec<Address>, Vec<u64>, Vec<U256>);

/// The pair an S-curve entry belongs to.
pub(super) fn entry_pair(oracle: &OracleContract, idx: u32) -> Result<AddressPair> {
    oracle.scurve_pair.read_pair(&idx)
}

/// Returns the S-curve value for a pair at a given timestamp.
///
/// The function keeps its public name for ABI compatibility. Fresh state stores
/// at most one continuous entry per pair.
pub fn get_max_active_scurve_value(
    oracle: &OracleContract,
    pair: AddressPair,
    timestamp: u64,
) -> Result<U256> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;
    let target_day = truncate_to_day(timestamp);

    let mut max_value = U256::ZERO;

    for idx in oldest..count {
        if entry_pair(oracle, idx)? != pair {
            continue;
        }

        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;

        if target_day < peak_day {
            continue;
        }

        let days_since = ((target_day - peak_day) / DAY_SECONDS) as usize;
        let value = compute_scurve_value(peak_price, days_since);
        if value > max_value {
            max_value = value;
        }
    }

    Ok(max_value)
}

/// Returns the continuous S-curve entry for a specific pair.
///
/// Returns `(peak_days, peak_prices, current_values)` as parallel arrays.
pub fn get_scurve_entries(
    oracle: &OracleContract,
    pair: AddressPair,
    current_timestamp: u64,
) -> Result<(Vec<u64>, Vec<U256>, Vec<U256>)> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;
    let target_day = truncate_to_day(current_timestamp);

    let mut peak_days = Vec::new();
    let mut peak_prices = Vec::new();
    let mut current_values = Vec::new();

    for idx in oldest..count {
        if entry_pair(oracle, idx)? != pair {
            continue;
        }

        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;

        let days_since = if target_day >= peak_day {
            ((target_day - peak_day) / DAY_SECONDS) as usize
        } else {
            continue;
        };

        peak_days.push(peak_day);
        peak_prices.push(peak_price);
        current_values.push(compute_scurve_value(peak_price, days_since));
    }

    Ok((peak_days, peak_prices, current_values))
}

/// Returns all S-curve data across all pairs.
///
/// Returns `(bases, quotes, peak_days, peak_prices)` as parallel arrays.
pub fn get_all_scurve_data(oracle: &OracleContract) -> Result<ScurveTable> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;

    let mut bases = Vec::new();
    let mut quotes = Vec::new();
    let mut peak_days = Vec::new();
    let mut peak_prices = Vec::new();

    for idx in oldest..count {
        let pair = entry_pair(oracle, idx)?;
        let peak_day = oracle.scurve_peak_day.read(&idx)?;
        let peak_price = oracle.scurve_peak_price.read(&idx)?;

        bases.push(pair.address1());
        quotes.push(pair.address2());
        peak_days.push(peak_day);
        peak_prices.push(peak_price);
    }

    Ok((bases, quotes, peak_days, peak_prices))
}

/// Returns all S-curve data for a specific pair.
///
/// Returns `(peak_days, peak_prices)` as parallel arrays. The full continuous
/// value chain is deterministic from the peak price, so callers can use
/// `getScurveValues` for timestamp-specific values.
pub fn get_all_scurve_data_for_pair(
    oracle: &OracleContract,
    pair: AddressPair,
) -> Result<(Vec<u64>, Vec<U256>)> {
    let count = oracle.scurve_count.read()?;
    let oldest = oracle.scurve_oldest_idx.read()?;

    let mut peak_days = Vec::new();
    let mut peak_prices = Vec::new();

    for idx in oldest..count {
        if !entry_pair(oracle, idx)?.same_market(&pair) {
            continue;
        }
        peak_days.push(oracle.scurve_peak_day.read(&idx)?);
        peak_prices.push(oracle.scurve_peak_price.read(&idx)?);
    }

    Ok((peak_days, peak_prices))
}
