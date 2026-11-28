use alloy_primitives::{Address, U256};
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{MutationPurposeTag, StorageHandle};

use crate::precompile::IMetadosis;
use outbe_emission::schema::metadosis_state;
use outbe_worldwideday::{
    precompile::{dispatch as worldwide_day_dispatch, IWorldwideDay},
    WorldwideDayContract,
};

const CHAIN_ID: u64 = 1;

fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 64);
    outbe_nod::test_support::install();
    StorageHandle::enter(&mut storage, |storage| f(storage.clone()))
}

/// Starts `day` as the Cycle does, with all of its limit to Metadosis.
fn allocate_to_metadosis(ctx: &BlockRuntimeContext<'_>, day: u32, limit: U256) {
    let mut record = outbe_emission::api::get_emission_day(ctx.storage.clone(), day)
        .unwrap()
        .unwrap_or_else(|| {
            outbe_emission::EmissionDay::new(
                day,
                outbe_system_common::time::WorldwideDay::new(0),
                outbe_emission::api::compute_metadosis_start(day).unwrap(),
                0,
                [0; 5],
            )
        });
    let limit = u128::try_from(limit).unwrap();
    record.emission_limit = limit;
    record.validators_limit = 0;
    record.cca_limit = 0;
    record.waa_limit = 0;
    record.sra_limit = 0;
    record.metadosis_limit = limit;
    record.state = outbe_emission::schema::emission_day_state::PROCESSING;
    outbe_emission::api::put_emission_day(ctx.storage.clone(), &record).unwrap();
}

fn install_test_enclaves() {
    outbe_nod::test_support::install();
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
mod day_loads;
mod emission_day_lifecycle;
mod precompile;
