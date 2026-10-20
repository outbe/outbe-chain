use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::dsl::StorageRecord;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{MetadosisMutationPurposeTag, StorageHandle};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::constants::*;
use crate::fixture_kernel::FixtureKernelExt;
use crate::precompile::{dispatch as metadosis_dispatch, IMetadosis};
use crate::runtime::timestamp_to_date_key;
use crate::schema::{status, MetadosisContract, WorldwideDay, WorldwideDayEntryExt};

const CHAIN_ID: u64 = 1;

fn with_contract<R>(f: impl FnOnce(&mut MetadosisContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    outbe_fidelity::enclave_client::test_enclave::install();
    outbe_tribute::enclave_client::test_enclave::install();
    StorageHandle::enter(&mut storage, |storage| {
        let mut contract = MetadosisContract::new(storage.clone());
        f(&mut contract)
    })
}

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 64);
    outbe_fidelity::enclave_client::test_enclave::install();
    outbe_tribute::enclave_client::test_enclave::install();
    StorageHandle::enter(&mut storage, |storage| f(storage.clone()))
}

fn install_test_enclaves() {
    outbe_fidelity::enclave_client::test_enclave::install();
    outbe_tribute::enclave_client::test_enclave::install();
}

fn form_due_fixture_day_limits(storage: &StorageHandle, timestamp: u64) {
    let mut metadosis = MetadosisContract::new(storage.clone());
    for wwd in metadosis.active_wwd.read_all().unwrap() {
        let day = metadosis.worldwide_days.entry(wwd);
        let current = day.status().read().unwrap();
        if current < status::OFFERING
            && timestamp >= day.offering_end().read().unwrap()
            && metadosis.day_limit_formation(wwd).unwrap().is_none()
        {
            metadosis.fixture_seed_day_limit_formation(wwd).unwrap();
        }
    }
}

/// Price the day-type currency, which genesis lists as a reference: the brief needs a
/// price for it.
fn arm_reference_price(storage: &StorageHandle, timestamp: u64) {
    use outbe_oracle::schema::OracleContract;
    let index = match outbe_oracle::api::require_coen_pair(
        storage.clone(),
        outbe_oracle::constants::DAY_TYPE_ISO,
    ) {
        Ok((_, index)) => index,
        Err(_) => {
            outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
                .unwrap()
        }
    };
    let oracle = OracleContract::new(storage.clone());
    if oracle.reference_currencies.len().unwrap() == 0 {
        oracle
            .reference_currencies
            .push(outbe_oracle::constants::DAY_TYPE_ISO)
            .unwrap();
    }
    let last_closed = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(timestamp),
    );
    oracle
        .record_utc_day_vwap(last_closed, index, U256::from(100u64))
        .unwrap();
}

/// Drive the WWD lifecycle the way the daily Cycle handler does: invoke `start_metadosis`
/// on a synthetic context. These tests exercise the state machine sub-day, so they call it
/// directly rather than through a per-block hook.
fn run_begin_block_with_chain_id(
    storage: StorageHandle,
    block_number: u64,
    timestamp: u64,
    chain_id: u64,
) {
    install_test_enclaves();
    arm_reference_price(&storage, timestamp);
    // The direct lifecycle fixture omits Cycle's daily allocation transaction.
    // Before a day can exercise the MissedOffering branch, model that preceding
    // production step by sealing its already-written limit as the immutable
    // day-limit formation.
    form_due_fixture_day_limits(&storage, timestamp);
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, chain_id),
        storage,
    );
    crate::commands::start_metadosis(&ctx).unwrap();
}

fn run_begin_block(storage: StorageHandle, block_number: u64, timestamp: u64) {
    run_begin_block_with_chain_id(
        storage,
        block_number,
        timestamp,
        outbe_primitives::chain::CHAIN_ID,
    );
}

mod capacity;
mod lifecycle;
mod lysis_ingress;
mod onchain_lysis;
mod precompile;
mod reducer;
mod state;
mod wwd_vwap_overflow;

mod tribute_fixture;

#[cfg(feature = "test-utils")]
mod api;
