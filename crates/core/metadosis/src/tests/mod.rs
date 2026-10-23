use alloy_primitives::{Address, B256, U256};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{MetadosisMutationPurposeTag, StorageHandle};
use outbe_tribute::TributeContract;

use crate::precompile::{dispatch as metadosis_dispatch, IMetadosis};
use crate::schema::{emission_day_state, MetadosisContract};
use outbe_tribute::precompile::{dispatch as tribute_dispatch, ITribute};

const CHAIN_ID: u64 = 1;

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
    )
    .expect("date step");
    oracle
        .record_utc_day_vwap(last_closed, index, U256::from(100u64))
        .unwrap();
}

mod api;
mod emission_day_lifecycle;
mod onchain_lysis;
mod precompile;
