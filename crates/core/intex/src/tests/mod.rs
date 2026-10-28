//! Shared fixtures for the Intex tests.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::time::SECONDS_PER_DAY_U32;

use crate::api;
use crate::config;
use crate::precompile::{dispatch, IIntex};
use crate::schema::{CreateSeriesParams, IntexCallTrigger, IntexState, SeriesId};
use crate::IntexContract;

mod group_index;
mod layout;
mod precompile;
mod series;
mod series_id;
mod units;

const CHAIN_ID: u64 = 1;

const ISSUED_AT: u32 = 1_700_000_000;

const PROMIS_LOAD_MINOR: u128 = 1_000_000; // 1 PROMIS in PROMIS-unit

const CALL_NOTICE_PERIOD: u32 = 7 * SECONDS_PER_DAY_U32;
const TRANSIT_MARGIN: u32 = 3_600;

const ENTRY_PRICE_MINOR: u64 = 2_000_000;

const CALL_PRICE_MINOR: u64 = 900_000;

fn with_registry<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT as u64));
    StorageHandle::enter(&mut storage, f)
}

/// The owner the unit ledgers are booked against in these tests.
fn owner() -> Address {
    Address::repeat_byte(0xA1)
}

/// Test ids carry a fixed USD/U pair. Only the day varies.
fn sid(worldwide_day: u32) -> SeriesId {
    SeriesId::pack(WorldwideDay::new(worldwide_day), *b"USD", b'U').unwrap()
}

fn sample_params(worldwide_day: u32) -> CreateSeriesParams {
    CreateSeriesParams {
        series_id: sid(worldwide_day),
        worldwide_day: WorldwideDay::new(worldwide_day),
        issued_units: 100,
        promis_load_minor: PROMIS_LOAD_MINOR,
        entry_price_minor: U256::from(ENTRY_PRICE_MINOR),
        call_price_minor: U256::from(CALL_PRICE_MINOR),
        call_trigger: IntexCallTrigger {
            call_window_seconds: 30 * SECONDS_PER_DAY_U32,
            call_threshold_seconds: 5 * SECONDS_PER_DAY_U32,
            call_notice_period_seconds: CALL_NOTICE_PERIOD,
            transit_margin_seconds: TRANSIT_MARGIN,
        },
        issued_at: ISSUED_AT,
        issuance_currency: 840,
        reference_currency: 840,
    }
}
