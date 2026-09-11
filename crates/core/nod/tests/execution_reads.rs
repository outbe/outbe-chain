use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    begin_block, EntityRef, ExecutionScope, IdPage, IdPageRequest, ParentBodySource,
    ParentBodySourceError, QueryRef, StoredBody, WwdEntityId,
};
use outbe_nod::{
    api, constants::MAX_BUCKET_QUALIFICATIONS_PER_RUN, hooks, precompile::INod, NodContract,
    NodItemState, NodRepositoryReader,
};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS},
    block::{BlockContext, BlockLifecycle, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn item(owner: Address, day: WorldwideDay) -> NodItemState {
    let nod_id = NodContract::generate_nod_id(owner, day).unwrap();
    NodItemState {
        nod_id,
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day: day,
        league_id: 4,
        floor_price_minor: U256::from(13),
        bucket_key: NodContract::bucket_key(day, U256::from(13), 978),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_752_534_000,
    }
}

fn active_world() -> (HashMapStorageProvider, ExecutionScope, NodRepositoryReader) {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage;
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
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
    (provider, scope, NodRepositoryReader::new(reader))
}

#[test]
fn same_block_issue_is_visible_to_point_and_list_reads() {
    let (mut provider, scope, parent) = active_world();
    let body = item(Address::repeat_byte(0x21), WorldwideDay::new(20_260_716));

    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        assert_eq!(
            api::get_item(&storage, &scope, &parent, body.nod_id)
                .unwrap()
                .unwrap()
                .owner,
            body.owner
        );
        assert_eq!(
            api::list_by_owner(&storage, &scope, &parent, body.owner)
                .unwrap()
                .into_iter()
                .map(|item| item.nod_id)
                .collect::<Vec<_>>(),
            [body.nod_id]
        );
        assert_eq!(api::list_all(&storage, &scope, &parent).unwrap().len(), 1);
    });

    let signatures: Vec<_> = provider
        .get_events(NOD_ADDRESS)
        .iter()
        .map(|event| event.topics()[0])
        .collect();
    assert_eq!(
        signatures,
        [
            INod::NodBodyStored::SIGNATURE_HASH,
            INod::NodBucketBodyStored::SIGNATURE_HASH,
        ]
    );
}

#[test]
fn qualification_updates_the_overlay_and_keeps_the_product_event() {
    let (mut provider, scope, parent) = active_world();
    let body = item(Address::repeat_byte(0x31), WorldwideDay::new(20_260_716));
    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        let midnight = outbe_primitives::time::date_key_to_utc_timestamp(20260716);
        let context = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, midnight, 1),
            storage.clone(),
        );
        let mut oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(body.reference_currency);
        outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
        oracle
            .reference_currencies
            .push(body.reference_currency)
            .unwrap();
        oracle.config_is_initialized.write(true).unwrap();
        // The completed day's weighted price is (12*1 + 15*2)/3 = 14.
        // The new day's low sample is outside the half-open daily window.
        for (timestamp, rate, volume) in [
            (midnight - 200, 12, 1),
            (midnight - 100, 15, 2),
            (midnight, 1, 1),
        ] {
            oracle
                .write_snapshot(timestamp, &[(pair, U256::from(rate), U256::from(volume))])
                .unwrap();
        }
        let bucket_id = WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key.0);
        hooks::run_daily(&context, &scope, &parent).unwrap();
        assert!(
            !api::get_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap()
                .is_qualified
        );
        outbe_oracle::lifecycle::OracleLifecycle::begin_block(&context).unwrap();
        assert_eq!(
            oracle
                .get_utc_day_vwap_for_pair(20260715, oracle.pair_index_of(pair).unwrap())
                .unwrap(),
            Some(U256::from(14))
        );
        hooks::run_daily(&context, &scope, &parent).unwrap();
        assert!(
            api::get_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap()
                .is_qualified
        );

        // A different reference currency walks its own trie and sees nothing,
        // even though the floor value is identical.
        assert_eq!(
            hooks::qualify_buckets_with_rate(
                &context,
                &scope,
                &parent,
                840,
                body.floor_price_minor + U256::from(1),
                MAX_BUCKET_QUALIFICATIONS_PER_RUN,
            )
            .unwrap(),
            0
        );
    });
    assert!(provider
        .get_events(NOD_ADDRESS)
        .iter()
        .any(|event| event.topics()[0] == INod::NodBucketQualified::SIGNATURE_HASH));
}

#[test]
fn qualification_takes_only_own_currency_buckets_strictly_below_the_rate() {
    let (mut provider, scope, parent) = active_world();
    let day = WorldwideDay::new(20_260_716);
    // (owner byte, reference currency, floor price).
    let specs = [
        (0x51u8, 840u16, 1000u64),
        (0x52, 978, 1200),
        (0x53, 840, 1250),
        (0x54, 840, 1300),
        (0x55, 978, 1300),
        // Shares the rate's bin, so it exercises the tail-bin exact compare.
        (0x56, 840, 1299),
    ];
    // Only the 840 buckets strictly below 1299 qualify: 1299/1300 are at or
    // above the rate and the 978 buckets belong to another currency's trie.
    let expected = [true, false, true, false, false, false];

    StorageHandle::enter(&mut provider, |storage| {
        let bucket_ids: Vec<WwdEntityId> = specs
            .iter()
            .map(|&(owner_byte, currency, floor)| {
                let floor = U256::from(floor);
                let mut body = item(Address::repeat_byte(owner_byte), day);
                body.floor_price_minor = floor;
                body.reference_currency = currency;
                body.bucket_key = NodContract::bucket_key(day, floor, currency);
                api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
                WwdEntityId::from_day_and_digest(day, body.bucket_key.0)
            })
            .collect();

        let context = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, 1_752_534_000, 1),
            storage.clone(),
        );
        hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            840,
            U256::from(1299),
            MAX_BUCKET_QUALIFICATIONS_PER_RUN,
        )
        .unwrap();

        let qualified: Vec<bool> = bucket_ids
            .iter()
            .map(|&bucket_id| {
                api::get_bucket(&storage, &scope, &parent, bucket_id)
                    .unwrap()
                    .unwrap()
                    .is_qualified
            })
            .collect();
        assert_eq!(qualified, expected);
    });
}

struct CountingParent {
    inner: NodRepositoryReader,
    gets: AtomicUsize,
}

impl ParentBodySource for CountingParent {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        ParentBodySource::get(&self.inner, entity)
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> Result<IdPage, ParentBodySourceError> {
        ParentBodySource::list(&self.inner, query, request)
    }
}

#[test]
fn removal_consumes_loaded_capabilities_without_a_second_parent_read() {
    let (mut provider, scope, reader) = active_world();
    let parent = CountingParent {
        inner: reader,
        gets: AtomicUsize::new(0),
    };
    let body = item(Address::repeat_byte(0x41), WorldwideDay::new(20_260_716));
    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        let loaded_item = api::load_item(&storage, &scope, &parent, body.nod_id)
            .unwrap()
            .unwrap();
        let bucket_id = WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key.0);
        let loaded_bucket = api::load_bucket(&storage, &scope, &parent, bucket_id)
            .unwrap()
            .unwrap();
        let reads_before_remove = parent.gets.load(Ordering::SeqCst);
        api::remove_nod(&storage, &scope, loaded_item, loaded_bucket).unwrap();
        assert_eq!(parent.gets.load(Ordering::SeqCst), reads_before_remove);
        assert!(api::get_item(&storage, &scope, &parent, body.nod_id)
            .unwrap()
            .is_none());
    });
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
            .utc_day_vwap_value
            .get_nested(&20260715)
            .write(&index, U256::from(13))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(20260715).unwrap();
        for (owner, floor) in [(0x51, 12), (0x52, 13)] {
            let mut body = item(Address::repeat_byte(owner), WorldwideDay::new(20260715));
            body.floor_price_minor = U256::from(floor);
            body.bucket_key =
                NodContract::bucket_key(body.worldwide_day, body.floor_price_minor, 978);
            api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        }
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, midnight, 1),
            storage.clone(),
        );
        hooks::run_daily(&ctx, &scope, &parent).unwrap();
        assert_eq!(NodContract::new(storage).callable_buckets.len().unwrap(), 1);
    });
    // One bucket is at the qualification floor; the other is qualified but
    // below its call price. Neither unchanged scan should issue an SSTORE.
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, midnight, 1), storage);
        hooks::run_daily(&ctx, &scope, &parent).unwrap();
    });
    let (reads, writes) = provider.metered_storage_operations();
    assert!(reads > 0);
    assert_eq!(writes, 0);
}
