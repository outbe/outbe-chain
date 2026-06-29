use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::dsl::StorageRecord;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::{TributeContract, TributeData};

use crate::constants::*;
use crate::precompile::{dispatch as metadosis_dispatch, IMetadosis};
use crate::runtime::timestamp_to_date_key;
use crate::schema::{
    day_type, status, DayType, MetadosisContract, Status, WorldwideDay, WorldwideDayEntryExt,
};

const CHAIN_ID: u64 = 1;

fn with_contract<R>(f: impl FnOnce(&mut MetadosisContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = MetadosisContract::new(storage.clone());
        f(&mut contract)
    })
}

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| f(storage.clone()))
}

/// Seed an active, sealed WorldwideDay at `forming_start` using production
/// lookback/offering hours — the `create_worldwide_day` + `add_active_wwd` +
/// `seal_day` triple every lifecycle test otherwise inlines.
fn seed_active_day(storage: &StorageHandle, wwd: outbe_common::WorldwideDay, forming_start: u64) {
    let mut metadosis = MetadosisContract::new(storage.clone());
    metadosis
        .create_worldwide_day(
            wwd,
            forming_start,
            LOOKBACK_DELAY_HOURS,
            OFFERING_PERIOD_HOURS,
        )
        .unwrap();
    metadosis.add_active_wwd(wwd).unwrap();
    TributeContract::new(storage.clone()).seal_day(wwd).unwrap();
}

/// Force a day into the WAITING phase with a resolved day type — the
/// `set_wwd_day_type + status(WAITING)` setup the settlement tests share before
/// driving READY. The metadosis limit is left to the caller (some tests assert on
/// a missing/zero limit); the day must already exist (see `seed_active_day`).
fn mark_day_waiting(storage: &StorageHandle, wwd: outbe_common::WorldwideDay, day_type: DayType) {
    let mut metadosis = MetadosisContract::new(storage.clone());
    metadosis.set_wwd_day_type(wwd, day_type).unwrap();
    metadosis.write_status(wwd, Status::Waiting).unwrap();
}

/// Seed the Oracle COEN/0xUSD pair with a previous-day VWAP (at
/// `previous_forming_start + 1h`) and a current-day VWAP (at `current_snapshot_ts`),
/// then store the previous day's snapshot — the oracle setup the VWAP/day-rate
/// tests share. Returns the registered pair id (some tests assert on it).
#[allow(clippy::too_many_arguments)]
fn seed_oracle_vwap(
    storage: &StorageHandle,
    previous_wwd: outbe_common::WorldwideDay,
    previous_forming_start: u64,
    previous_forming_end: u64,
    current_snapshot_ts: u64,
    previous_vwap: U256,
    current_vwap: U256,
) -> u32 {
    let mut oracle = OracleContract::new(storage.clone());
    let pair_id = oracle.register_pair("COEN", "0xUSD").unwrap();
    oracle
        .write_snapshot(
            previous_forming_start + SECONDS_PER_HOUR,
            &[(pair_id, previous_vwap, U256::from(1u64))],
        )
        .unwrap();
    oracle
        .write_snapshot(
            current_snapshot_ts,
            &[(pair_id, current_vwap, U256::from(1u64))],
        )
        .unwrap();
    oracle
        .store_worldwide_day_vwap_snapshot(
            previous_wwd,
            previous_forming_start,
            previous_forming_end,
        )
        .unwrap();
    pair_id
}

/// Drive the WWD lifecycle the way the daily Cycle handler does:
/// invoke `start_metadosis` on a synthetic context. Production no
/// longer drives Metadosis through a per-block lifecycle hook (see
/// ), but these tests intentionally exercise the state
/// machine sub-day, so they call `start_metadosis` directly.
fn run_begin_block_with_chain_id(
    storage: StorageHandle,
    block_number: u64,
    timestamp: u64,
    chain_id: u64,
) {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, chain_id),
        storage,
    );
    crate::runtime::start_metadosis(&ctx).unwrap();
}

fn run_begin_block(storage: StorageHandle, block_number: u64, timestamp: u64) {
    run_begin_block_with_chain_id(
        storage,
        block_number,
        timestamp,
        outbe_primitives::chain::CHAIN_ID,
    );
}

/// Like `run_begin_block`, but returns the result instead of unwrapping, so
/// tests can assert that a terminal failure propagates out of the begin-zone
/// system transaction instead of being silently retired.
fn try_run_begin_block(
    storage: StorageHandle,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, outbe_primitives::chain::CHAIN_ID),
        storage,
    );
    crate::runtime::start_metadosis(&ctx)
}

mod lifecycle;
mod state;
