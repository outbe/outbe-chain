//! Oracle state fixtures for tests in other crates.
//!
//! Each function makes the same Oracle storage writes, in the same order, as
//! the Oracle API calls that it replaces. Each function returns the first
//! error. The caller decides how the test fails.

use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::api::{
    current_vwap_snapshot, register_pair, set_exchange_rate, AddressPair, RateObservation,
};
use crate::constants::{DAY_TYPE_PAIR, VWAP_HOUR_SECONDS};
use crate::schema::OracleContract;

/// Registers the COEN/840 pair. Then publishes `rate` as the COEN/840 rate
/// that the zero address observed in block `block_number` at `timestamp`.
pub fn publish_day_type_rate(
    storage: &StorageHandle<'_>,
    rate: U256,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    register_pair(storage.clone(), DAY_TYPE_PAIR)?;
    set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        DAY_TYPE_PAIR,
        RateObservation {
            rate,
            block_number,
            timestamp,
        },
    )
}

/// Registers COEN/`iso` if it is new, records `vwap` for UTC day `day` and
/// moves the last finalized day up to `day`.
pub fn record_finalized_day_vwap(
    storage: &StorageHandle<'_>,
    iso: u16,
    day: u32,
    vwap: U256,
) -> Result<()> {
    let pair = AddressPair::new_coen_to(iso);
    let oracle = OracleContract::new(storage.clone());
    let mut index = oracle.pair_index_of(pair)?;
    if index == 0 {
        index = register_pair(storage.clone(), pair)?;
    }
    oracle.record_utc_day_vwap(day, index, vwap)?;
    if oracle.utc_day_vwap_last_finalized.read()? < day {
        oracle.utc_day_vwap_last_finalized.write(day)?;
    }
    Ok(())
}

/// Registers COEN/840 when absent and COEN/`iso`. Then covers the current
/// trailing window with COEN at `usd_vwap` against ISO 840 and at `iso_vwap`
/// against `iso`, each with unit volume (see [`seed_covered_trailing_window`]).
pub fn seed_trailing_vwap_window(
    storage: &StorageHandle<'_>,
    iso: u16,
    usd_vwap: U256,
    iso_vwap: U256,
) -> Result<()> {
    let usd = AddressPair::new_coen_to(840);
    let other = AddressPair::new_coen_to(iso);
    if OracleContract::new(storage.clone()).pair_index_of(usd)? == 0 {
        register_pair(storage.clone(), usd)?;
    }
    register_pair(storage.clone(), other)?;
    let volume = U256::from(1_000_000);
    seed_covered_trailing_window(storage, usd, usd_vwap, volume)?;
    seed_covered_trailing_window(storage, other, iso_vwap, volume)
}

/// Gives `pair` a covered current trailing VWAP window at `rate`:
/// - notes the first block of every window hour and of the cutoff, one vote
///   period apart, so each hour allows one round;
/// - adds one observation to the hourly aggregate of each hour but the last,
///   as `write_snapshot` does;
/// - writes the last observation as a snapshot just before the cutoff.
///
/// The earlier hours skip the snapshot list, so a later call for another pair
/// keeps the snapshot order.
pub fn seed_covered_trailing_window(
    storage: &StorageHandle<'_>,
    pair: AddressPair,
    rate: U256,
    volume: U256,
) -> Result<()> {
    let snapshot = current_vwap_snapshot(storage.clone())?;
    let mut oracle = OracleContract::new(storage.clone());
    let vote_period = oracle.config_vote_period.read()?.max(1);
    let (mut hour, mut block) = (snapshot.start(), 1);
    while hour <= snapshot.cutoff() {
        oracle.record_hour_block(hour, block)?;
        if hour + VWAP_HOUR_SECONDS < snapshot.cutoff() {
            oracle.accumulate_hourly_vwap(pair, hour, rate.saturating_mul(volume), volume)?;
        }
        hour += VWAP_HOUR_SECONDS;
        block += vote_period;
    }
    oracle.write_snapshot(snapshot.cutoff() - 1, &[(pair, rate, volume)])
}

/// Reopens a completed daily call sweep on `day` so a test can arm a closed day.
#[macro_export]
macro_rules! reopen_daily_call_sweep {
    ($contract:expr, $storage:expr, $day:expr) => {{
        let contract = $contract;
        let day = $day;
        if contract.daily_genesis_day.read()? == 0 || day < contract.daily_genesis_day.read()? {
            contract.daily_genesis_day.write(day)?;
        }
        if day > contract.call_latest_day.read()? {
            contract.call_latest_day.write(day)?;
        }
        contract.call_sweep_day.write(day)?;
        contract.daily_phase.write(0)?;
        contract.call_currency_cursor.write(0)?;
        for currency in outbe_oracle::api::REFERENCE_CURRENCIES {
            contract.call_bin_cursor.write(&currency, 0)?;
            contract.call_scan_failed_day.clear(&currency)?;
        }
        Ok(())
    }};
}
