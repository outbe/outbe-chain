//! Writes of the S-curve chains and daily peak detection over closed days.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;

use super::query::entry_pair;
use super::{compute_scurve_value, truncate_to_day, DAY_SECONDS};
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::OracleContract;

/// Stores or replaces the single S-curve chain for a pair.
pub fn store_scurve_entry(
    oracle: &mut OracleContract,
    pair: AddressPair,
    peak_day: u64,
    peak_price: U256,
) -> Result<()> {
    if oracle.ocomp_profile_ready.read()? {
        let storage = oracle.storage.clone();
        storage
            .with_checkpoint(|| store_scurve_entry_inner(oracle, pair, peak_day, peak_price))
            .map(|_| ())
    } else {
        store_scurve_entry_inner(oracle, pair, peak_day, peak_price).map(|_| ())
    }
}

fn store_scurve_entry_inner(
    oracle: &mut OracleContract,
    pair: AddressPair,
    peak_day: u64,
    peak_price: U256,
) -> Result<bool> {
    let count = oracle.scurve_count.read()?;
    let next_idx = count
        .checked_add(1)
        .ok_or(OracleError::ScurveWriteIndexOverflow)?;
    let oldest = oracle.scurve_oldest_idx.read()?;
    for idx in oldest..count {
        if entry_pair(oracle, idx)? != pair {
            continue;
        }

        let current_peak_day = oracle.scurve_peak_day.read(&idx)?;
        if peak_day < current_peak_day {
            return Ok(false);
        }
        let days_since = ((peak_day - current_peak_day) / DAY_SECONDS) as usize;
        let current_value = compute_scurve_value(oracle.scurve_peak_price.read(&idx)?, days_since);
        if peak_price <= current_value {
            return Ok(false);
        }

        let next_ocomp_version = oracle.next_ocomp_state_version()?;
        oracle.scurve_peak_day.write(&idx, peak_day)?;
        oracle.scurve_peak_price.write(&idx, peak_price)?;
        oracle.commit_ocomp_state_version(next_ocomp_version)?;
        return Ok(true);
    }

    let idx = count;
    let next_ocomp_version = oracle.next_ocomp_state_version()?;
    oracle.scurve_pair.write_pair(&idx, pair)?;
    oracle.scurve_peak_day.write(&idx, peak_day)?;
    oracle.scurve_peak_price.write(&idx, peak_price)?;
    oracle.scurve_count.write(next_idx)?;
    oracle.commit_ocomp_state_version(next_ocomp_version)?;
    Ok(true)
}

/// Retained public seam. Continuous chains never expire or advance `oldest`.
pub fn evict_expired_scurves(_oracle: &mut OracleContract, _current_timestamp: u64) -> Result<()> {
    Ok(())
}

/// Detects peaks from the last 3 *closed* daily close prices for a pair
/// and stores new S-curve entries.
///
/// A peak occurs when: close[D-3] < close[D-2] > close[D-1], i.e. D-2 is the
/// peak. The function never uses the current (just-started) day as a close.
/// Thus the function confirms the peak of a day X at the start of X+2.
///
/// The daily hook calls this function on the first block of each UTC day.
pub fn process_daily_scurve(
    oracle: &mut OracleContract,
    pair: AddressPair,
    timestamp: u64,
) -> Result<()> {
    if oracle.ocomp_profile_ready.read()? {
        let storage = oracle.storage.clone();
        storage.with_checkpoint(|| process_daily_scurve_inner(oracle, pair, timestamp))
    } else {
        process_daily_scurve_inner(oracle, pair, timestamp)
    }
}

fn process_daily_scurve_inner(
    oracle: &mut OracleContract,
    pair: AddressPair,
    timestamp: u64,
) -> Result<()> {
    let current_day = truncate_to_day(timestamp);

    // The daily hook fires on the first block of `current_day`, so
    // `current_day` itself has no close yet. Detect peaks only over fully
    // CLOSED UTC days. At this point the most recent closed day is D-1, so
    // the latest peak we can confirm is D-2. To confirm a peak, we need the
    // close of the day that follows it.
    //
    //   day_minus_3 (close before peak) < day_minus_2 (peak) > day_minus_1 (close after peak)
    let day_minus_1 = current_day.saturating_sub(DAY_SECONDS);
    let day_minus_2 = current_day.saturating_sub(2 * DAY_SECONDS);
    let day_minus_3 = current_day.saturating_sub(3 * DAY_SECONDS);

    // Last snapshot rate within each fully-closed UTC day.
    let close_d1 = get_daily_close(oracle, pair, day_minus_1)?;
    let close_d2 = get_daily_close(oracle, pair, day_minus_2)?;
    let close_d3 = get_daily_close(oracle, pair, day_minus_3)?;

    // Need all three closed-day prices to detect a peak.
    if close_d1.is_zero() || close_d2.is_zero() || close_d3.is_zero() {
        return Ok(());
    }

    // Peak detection: D-3 < D-2 > D-1 (i.e., D-2 is the peak).
    if close_d3 < close_d2
        && close_d2 > close_d1
        && store_scurve_entry_inner(oracle, pair, day_minus_2, close_d2)?
    {
        let event = IOracle::ScurvePeakDetected {
            base: pair.address1(),
            quote: pair.address2(),
            peakPrice: close_d2,
            peakDay: day_minus_2,
        };
        let event_result = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());
        if oracle.ocomp_profile_ready.read()? {
            event_result?;
        }
    }

    Ok(())
}

/// Gets the closest exchange rate snapshot for a pair on a given day.
///
/// Scans snapshots backwards from the day end to find the last rate for that day.
fn get_daily_close(oracle: &OracleContract, pair: AddressPair, day_start: u64) -> Result<U256> {
    let day_end = day_start + DAY_SECONDS;
    let write_idx = oracle.snapshot_write_idx.read()?;
    let oldest_idx = oracle.snapshot_oldest_idx.read()?;

    let mut idx = write_idx;
    while idx > oldest_idx {
        idx -= 1;
        let ts = oracle.snapshot_timestamp.read(&idx)?;
        if ts < day_start {
            break;
        }
        if ts >= day_end {
            continue;
        }

        // This snapshot is in the day. Look for our pair in it.
        let pc = oracle.snapshot_pair_count.read(&idx)?;
        let pair_map = oracle.snapshot_pair.get_nested(&idx);
        let rate_map = oracle.snapshot_rate.get_nested(&idx);

        for p in 0..pc {
            if pair_map.read_pair(&p)?.same_market(&pair) {
                return rate_map.read(&p);
            }
        }
    }

    Ok(U256::ZERO)
}
