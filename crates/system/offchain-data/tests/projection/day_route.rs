use std::sync::Arc;

use alloy_primitives::{Address, B256};
use outbe_nod::{NodPageRequest, NodRepositoryReader};
use outbe_offchain_data::{DayDatabaseRoute, FinalizedBlock, OffchainDataProjection};
use outbe_offchain_storage::{DayDatabases, Namespace, ScanRequest, StorageReader};
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_tribute::TributeRepositoryReader;

use super::support::*;

#[test]
fn new_tribute_and_nod_land_in_their_day_databases() {
    let dir = tempfile::tempdir().unwrap();
    let databases = Arc::new(DayDatabases::open(dir.path()).unwrap());
    let shared = Arc::new(databases.directory().open_shared().unwrap());
    let mut projection =
        OffchainDataProjection::open(config(5), shared.clone(), shared.clone()).unwrap();
    projection
        .set_day_route(DayDatabaseRoute {
            databases: databases.clone(),
            durable_reader: shared.clone(),
            durable_writer: shared.clone(),
        })
        .unwrap();

    let owner = Address::repeat_byte(0x11);
    let tribute_id = poseidon_entity(owner, 7);
    let body = tribute_body(tribute_id, owner, 7);
    let nod_id = poseidon_entity(owner, 20260715);
    projection
        .project_block(&FinalizedBlock {
            number: 5,
            hash: B256::repeat_byte(5),
            receipts: vec![receipt(
                0,
                1,
                vec![
                    log(
                        0,
                        TRIBUTE_ADDRESS,
                        tribute_stored_body_after(&body, B256::ZERO),
                    ),
                    log(
                        1,
                        NOD_ADDRESS,
                        nod_stored(nod_id, owner, B256::repeat_byte(0xbc)),
                    ),
                ],
            )],
        })
        .unwrap();

    for name in [
        "tributes",
        "tributes_by_owner",
        "tributes_by_day",
        "nods",
        "nod_buckets",
        "nods_by_owner",
    ] {
        let page = StorageReader::scan_prefix(
            shared.as_ref(),
            Namespace::new(name).unwrap(),
            ScanRequest::new(&[], None, 10).unwrap(),
        )
        .unwrap();
        assert!(page.entries.is_empty(), "{name}");
    }

    let tribute_day = databases.tribute_if_present(7).unwrap().unwrap();
    assert!(TributeRepositoryReader::new(tribute_day)
        .get(tribute_id)
        .unwrap()
        .is_some());
    let nod_day = databases.nod_if_present(20260715).unwrap().unwrap();
    assert!(NodRepositoryReader::new(nod_day)
        .get(nod_id)
        .unwrap()
        .is_some());
    assert!(databases.nod_if_present(7).unwrap().is_none());

    let nod_reader =
        NodRepositoryReader::with_days(shared.clone(), shared.clone(), databases.clone());
    let listed = nod_reader
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(listed.records.len(), 1);
    assert_eq!(listed.records[0].nod_id, nod_id);
    assert_eq!(owner_day_count(shared.as_ref()), 1);

    let bucket_key = B256::repeat_byte(0xbc);
    projection
        .project_block(&FinalizedBlock {
            number: 6,
            hash: B256::repeat_byte(6),
            receipts: vec![receipt(
                0,
                2,
                vec![log(0, NOD_ADDRESS, nod_deleted(nod_id, owner, bucket_key))],
            )],
        })
        .unwrap();
    let listed = nod_reader
        .list_by_owner(
            owner,
            NodPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert!(listed.records.is_empty());
    assert_eq!(owner_day_count(shared.as_ref()), 0);
}

fn owner_day_count(storage: &impl StorageReader) -> usize {
    StorageReader::scan_prefix(
        storage,
        Namespace::new("nod_owner_days").unwrap(),
        ScanRequest::new(&[], None, 10).unwrap(),
    )
    .unwrap()
    .entries
    .len()
}
