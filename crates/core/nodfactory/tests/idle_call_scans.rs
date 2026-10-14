//! An idle call scan reads and never writes: a CycleTick with nothing in flight
//! issues no SSTORE.

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{begin_block, ExecutionScope};
use outbe_nod::{api, NodContract, NodItemState, NodRepositoryReader};
use outbe_nodfactory::{called, hooks};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn item(owner: Address, day: WorldwideDay) -> NodItemState {
    let nod_id = outbe_nod::identity::generate_nod_id(owner, day).unwrap();
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: day,
            league_id: 4,
            bucket_key: outbe_nod::identity::bucket_key(day, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_752_534_000,
        },
        U256::from(5),
    )
}

fn active_world() -> (HashMapStorageProvider, ExecutionScope, NodRepositoryReader) {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage;
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::default();
    StorageHandle::enter(&mut provider, |storage| {
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
            .unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(
                    outbe_compressed_entities::sealed_root(B256::ZERO)
                        .unwrap()
                        .as_slice(),
                ),
            )
            .unwrap();
        begin_block(storage, &scope).unwrap();
    });
    (provider, scope, outbe_nod::nod_reader(reader))
}

#[test]
fn idle_daily_scans_do_not_write_storage() {
    let (mut provider, scope, parent) = active_world();
    let midnight = outbe_primitives::time::date_key_to_utc_timestamp(20260716);
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(978);
        let index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
        oracle.reference_currencies.push(978).unwrap();
        oracle
            .record_utc_day_vwap(20260715, index, U256::from(13))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(20260715).unwrap();
        for (owner, entry) in [(0x51, 12u64), (0x52, 13)] {
            let entry = U256::from(entry);
            let mut body = item(Address::repeat_byte(owner), WorldwideDay::new(20260715));
            body.bucket_key = outbe_nod::identity::bucket_key(body.worldwide_day, entry, 978);
            outbe_nod::test_support::set_entry_price(&mut body, entry);
            api::add_nod(
                &storage,
                outbe_compressed_entities::ExecutionReaders {
                    scope: &scope,
                    parent: &parent,
                },
                &body,
                entry,
                outbe_nod::test_support::sealed_terms(&storage, &body, entry),
            )
            .unwrap();
        }
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, midnight, 1),
            storage.clone(),
        );
        called::run_daily(&ctx).unwrap();
        hooks::sweep_forfeits(&ctx, &scope, &parent).unwrap();
        called::run_call_slice(&ctx).unwrap();
        assert_eq!(
            NodContract::new(storage).call_sweep_day.read().unwrap(),
            0,
            "both buckets wait below their call price, so the sweep closes at once"
        );
    });
    // An idle CycleTick only continues an in-flight sweep, so it issues no SSTORE.
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, midnight, 1), storage);
        hooks::sweep_forfeits(&ctx, &scope, &parent).unwrap();
        called::run_call_slice(&ctx).unwrap();
    });
    let (reads, writes) = provider.metered_storage_operations();
    assert!(reads > 0);
    assert_eq!(writes, 0);
}
