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
use crate::constants::{DAY_TYPE_ISO, DAY_TYPE_PAIR};
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

/// Does the steps of [`publish_day_type_rate`]. Then adds ISO 840 to the
/// reference currencies.
pub fn publish_day_type_quote(
    storage: &StorageHandle<'_>,
    rate: U256,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    publish_day_type_rate(storage, rate, block_number, timestamp)?;
    OracleContract::new(storage.clone())
        .reference_currencies
        .push(DAY_TYPE_ISO)
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

/// Registers COEN/840 when absent and COEN/`iso`. Then writes one observation just
/// before the current snapshot cutoff: COEN at `usd_vwap` against ISO 840 and at
/// `iso_vwap` against `iso`, each with unit volume.
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
    let snapshot = current_vwap_snapshot(storage.clone())?;
    let volume = U256::from(1_000_000);
    OracleContract::new(storage.clone()).write_snapshot(
        snapshot.cutoff() - 1,
        &[(usd, usd_vwap, volume), (other, iso_vwap, volume)],
    )
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
        for currency in outbe_oracle::schema::OracleContract::new($storage.clone())
            .reference_currencies
            .read_all()?
        {
            contract.call_bin_cursor.write(&currency, 0)?;
            contract.call_scan_failed_day.clear(&currency)?;
        }
        Ok(())
    }};
}
