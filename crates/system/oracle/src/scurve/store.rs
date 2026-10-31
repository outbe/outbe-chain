//! Writes of the per-pair S-curve ring.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::time::{WorldwideDay, SECONDS_PER_DAY};

use super::query::{curve_at, curve_value};
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::OracleContract;

/// Curves each pair keeps. The next curve overwrites the oldest one.
pub const SCURVE_RING_SLOTS: u64 = 32;

// A Tribute reads the curve in force at the oldest WWD still in an offering
// window, and every newer curve. That WWD started at most forming + lookback
// delay + offering ago, and a pair starts at most one curve for each WWD.
// The ring holds those curves plus the one in force before them.
// Genesis can only shorten these periods, so the defaults bound them.
const _: () = assert!(
    (outbe_chain_constants::DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS
        + outbe_chain_constants::DEFAULT_METADOSIS_LOOKBACK_DELAY_SECONDS
        + outbe_chain_constants::DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS)
        .div_ceil(SECONDS_PER_DAY)
        < SCURVE_RING_SLOTS
);

/// Starts a curve of `pair` at `wwd` with `peak_price` when the price is above
/// the newest curve at `wwd`. A `wwd` at or before the newest curve start is
/// ignored. Returns `true` when it stored the curve.
pub fn store_scurve_entry(
    oracle: &mut OracleContract,
    pair: AddressPair,
    wwd: WorldwideDay,
    peak_price: U256,
) -> Result<bool> {
    let storage = oracle.storage.clone();
    storage.with_checkpoint(|| store_scurve_entry_inner(oracle, pair, wwd, peak_price))
}

fn store_scurve_entry_inner(
    oracle: &mut OracleContract,
    pair: AddressPair,
    wwd: WorldwideDay,
    peak_price: U256,
) -> Result<bool> {
    let len = oracle.scurve_len.read(&pair)?;
    let next_len = len
        .checked_add(1)
        .ok_or(OracleError::ScurveWriteIndexOverflow)?;
    if let Some(newest) = len.checked_sub(1) {
        let start = oracle
            .scurve_peak_wwd
            .get_nested(&pair)
            .read(&(newest % SCURVE_RING_SLOTS))?;
        if wwd <= start {
            return Ok(false);
        }
    }
    if let Some((start, price)) = curve_at(oracle, pair, wwd)? {
        if peak_price <= curve_value(start, price, wwd)? {
            return Ok(false);
        }
    }
    let slot = len % SCURVE_RING_SLOTS;
    oracle.scurve_peak_wwd.get_nested(&pair).write(&slot, wwd)?;
    oracle
        .scurve_peak_price
        .get_nested(&pair)
        .write(&slot, peak_price)?;
    oracle.scurve_len.write(&pair, next_len)?;
    Ok(true)
}

/// Starts a curve at `wwd` for each pair in `wwd_vwaps` whose UTC day of the
/// same date has a VWAP. That UTC day is the middle 24 h of the forming window,
/// and it has a VWAP only when it passed the round coverage check.
pub(crate) fn process_wwd_scurves(
    oracle: &mut OracleContract,
    wwd: WorldwideDay,
    wwd_vwaps: &[(AddressPair, U256)],
) -> Result<()> {
    for &(pair, vwap) in wwd_vwaps {
        if !crate::constants::REFERENCE_CURRENCIES
            .iter()
            .any(|&iso_code| AddressPair::new_coen_to(iso_code) == pair)
        {
            continue;
        }
        let index = oracle.pair_index_of(pair)?;
        let covered = oracle
            .get_utc_day_vwap_for_pair(wwd.value(), index)?
            .is_some_and(|day_vwap| !day_vwap.is_zero());
        if covered && store_scurve_entry(oracle, pair, wwd, vwap)? {
            let event = IOracle::ScurvePeakDetected {
                base: pair.address1(),
                quote: pair.address2(),
                peakPrice: vwap,
                peakDay: wwd.into(),
            };
            oracle
                .storage
                .emit_event(ORACLE_ADDRESS, event.encode_log_data())?;
        }
    }
    Ok(())
}
