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
use outbe_nod::{api, precompile::INod, NodItemState, NodRepositoryReader};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, NOD_ADDRESS},
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
fn same_block_issue_is_visible_to_point_and_list_reads() {
    let (mut provider, scope, parent) = active_world();
    let body = item(Address::repeat_byte(0x21), WorldwideDay::new(20_260_716));

    StorageHandle::enter(&mut provider, |storage| {
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &body,
            U256::from(5),
            outbe_nod::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap();
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
            INod::Transfer::SIGNATURE_HASH,
        ]
    );
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
        api::add_nod(
            &storage,
            outbe_compressed_entities::ExecutionReaders {
                scope: &scope,
                parent: &parent,
            },
            &body,
            U256::from(5),
            outbe_nod::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap();
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
