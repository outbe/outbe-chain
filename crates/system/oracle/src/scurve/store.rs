//! Writes of the S-curve of a pair.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::error::Result;
use outbe_system_common::address_pair::AddressPair;
use outbe_system_common::addresses::ORACLE_ADDRESS;

use super::query::{curve, curve_value};
use crate::constants::VWAP_HOUR_SECONDS;
use crate::errors::OracleError;
use crate::precompile::IOracle;
use crate::schema::OracleContract;

/// Starts a curve of `pair` at the whole UTC hour `start_hour` with
/// `peak_price` when the price is above the current curve at that hour. The new
/// curve takes the place of the current one. A `start_hour` at or before the
/// start of the current curve is ignored. Returns `true` when it stored the
/// curve.
pub fn store_scurve_entry(
    oracle: &mut OracleContract,
    pair: AddressPair,
    start_hour: u64,
    peak_price: U256,
) -> Result<bool> {
    if start_hour == 0 || !start_hour.is_multiple_of(VWAP_HOUR_SECONDS) {
        return Err(OracleError::InvalidScurveStartHour.into());
    }
    if let Some((current_start, current_price)) = curve(oracle, pair)? {
        if start_hour <= current_start
            || peak_price <= curve_value(current_start, current_price, start_hour)
        {
            return Ok(false);
        }
    }
    // The two writes are one step: a failure of the second leaves no curve
    // with the new start and the old price.
    let storage = oracle.storage.clone();
    storage.with_checkpoint(|| {
        oracle.scurve_start_hour.write(&pair, start_hour)?;
        oracle.scurve_peak_price.write(&pair, peak_price)
    })?;
    Ok(true)
}

/// Starts a curve at `hour` for each reference currency pair in `vwaps` whose
/// 24-hour VWAP of that hour is above its current curve. A pair is in `vwaps`
/// only when its 24-hour window passed the round coverage check.
pub(crate) fn process_hourly_scurves(
    oracle: &mut OracleContract,
    hour: u64,
    vwaps: &[(AddressPair, U256)],
) -> Result<()> {
    for &(pair, vwap) in vwaps {
        if !crate::constants::REFERENCE_CURRENCIES
            .iter()
            .any(|&iso_code| AddressPair::new_coen_to(iso_code) == pair)
        {
            continue;
        }
        if store_scurve_entry(oracle, pair, hour, vwap)? {
            let event = IOracle::ScurvePeakDetected {
                base: pair.address1(),
                quote: pair.address2(),
                peakPrice: vwap,
                startHour: hour,
            };
            oracle
                .storage
                .emit_event(ORACLE_ADDRESS, event.encode_log_data())?;
        }
    }
    Ok(())
}
