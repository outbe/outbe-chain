use alloy_primitives::{Address, U256};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{MetadosisMutationPurposeTag, StorageHandle};
use outbe_tribute::TributeContract;

use crate::precompile::IMetadosis;
use crate::schema::MetadosisContract;
use outbe_emission::schema::metadosis_state;
use outbe_tributefactory::{
    precompile::{dispatch as tribute_factory_dispatch, ITributeFactory},
    TributeFactoryContract,
};

const CHAIN_ID: u64 = 1;

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 64);
    outbe_fidelity::enclave_client::test_enclave::install();
    outbe_tribute::enclave_client::test_enclave::install();
    StorageHandle::enter(&mut storage, |storage| f(storage.clone()))
}

/// Allocates `day` as the Cycle does, with all of its limit to Metadosis.
fn allocate_to_metadosis(ctx: &BlockRuntimeContext<'_>, day: u32, limit: U256) {
    crate::commands::set_metadosis_limit(ctx, day, limit).unwrap();
    outbe_emission::api::record_day_allocation(
        ctx.storage.clone(),
        day,
        outbe_emission::api::DayAllocationAmounts {
            emission_limit: limit,
            metadosis_limit: limit,
            validators: U256::ZERO,
            waa: U256::ZERO,
            sra: U256::ZERO,
            cca: U256::ZERO,
            promis_limit: U256::ZERO,
        },
    )
    .unwrap();
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
    let last_closed = outbe_system_common::time::previous_date_key(
        outbe_system_common::time::timestamp_to_date_key(timestamp),
    )
    .expect("date step");
    let day_before = outbe_system_common::time::previous_date_key(last_closed).expect("date step");
    for day in [day_before, last_closed] {
        oracle
            .record_utc_day_vwap(day, index, U256::from(100u64))
            .unwrap();
    }
}

mod api;
mod emission_day_lifecycle;
mod onchain_lysis;
mod precompile;
