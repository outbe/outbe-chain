mod conformance;

use std::{panic::AssertUnwindSafe, sync::Arc};

use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, Document},
    sync::Client,
};
use outbe_offchain_storage::{
    Key, MongoStorage, MongoStorageConfig, Namespace, StorageErrorKind, StorageReader,
    StorageReaderHandle, StorageWriter, StorageWriterHandle, Value, MAX_VALUE_BYTES,
};

macro_rules! mongo_conformance_test {
    ($name:ident) => {
        #[test]
        #[ignore = "requires OUTBE_TEST_MONGODB_URI"]
        fn $name() {
            run_isolated(stringify!($name), conformance::$name);
        }
    };
}

mongo_conformance_test!(put_get_replace_and_repeat);
mongo_conformance_test!(delete_is_idempotent_and_namespaces_are_isolated);
mongo_conformance_test!(scans_are_raw_byte_ordered_and_prefix_bounded);
mongo_conformance_test!(cursors_are_exclusive_and_traverse_multiple_pages);
mongo_conformance_test!(pages_are_bounded_by_total_value_bytes);
mongo_conformance_test!(cloned_handles_share_state_without_torn_values);
mongo_conformance_test!(maximum_key_value_and_entry_count_boundaries);

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn malformed_mongo_documents_are_corruption() {
    let uri = mongo_uri();
    let database = isolated_database_name("malformed_mongo_documents_are_corruption");
    let client = Client::with_uri_str(&uri).unwrap();
    let storage = MongoStorage::connect(MongoStorageConfig {
        uri,
        database: database.clone(),
    })
    .unwrap();
    let namespace = Namespace::new("corrupt_records").unwrap();
    let collection = client
        .database(&database)
        .collection::<Document>(namespace.as_str());

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        collection
            .insert_one(doc! { "_id": "aa", "value": "not binary" })
            .run()
            .unwrap();
        let error = storage
            .get(namespace.clone(), &Key::new([0xaa]).unwrap())
            .expect_err("wrong value type must be corruption");
        assert_eq!(error.kind(), StorageErrorKind::Corruption);
        collection.delete_many(doc! {}).run().unwrap();

        collection
            .insert_one(doc! {
                "_id": 7,
                "value": Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: vec![1],
                }),
            })
            .run()
            .unwrap();
        let request = outbe_offchain_storage::ScanRequest::new(b"a", None, 1).unwrap();
        let error = storage
            .scan_prefix(namespace.clone(), request)
            .expect_err("non-string _id must not disappear from a prefix scan");
        assert_eq!(error.kind(), StorageErrorKind::Corruption);
        collection.delete_many(doc! {}).run().unwrap();

        collection
            .insert_one(doc! {
                "_id": "aa",
                "value": Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: vec![1],
                }),
                "unexpected": true,
            })
            .run()
            .unwrap();
        let error = storage
            .get(namespace.clone(), &Key::new([0xaa]).unwrap())
            .expect_err("unexpected fields must be corruption");
        assert_eq!(error.kind(), StorageErrorKind::Corruption);
        collection.delete_many(doc! {}).run().unwrap();

        collection
            .insert_one(doc! {
                "_id": "aa",
                "value": Bson::Binary(Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes: vec![1; 16],
                }),
            })
            .run()
            .unwrap();
        let error = storage
            .get(namespace.clone(), &Key::new([0xaa]).unwrap())
            .expect_err("unexpected binary subtype must be corruption");
        assert_eq!(error.kind(), StorageErrorKind::Corruption);
        collection.delete_many(doc! {}).run().unwrap();

        collection
            .insert_one(doc! {
                "_id": "zz",
                "value": Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: vec![1],
                }),
            })
            .run()
            .unwrap();
        let request = outbe_offchain_storage::ScanRequest::new(&[], None, 1).unwrap();
        let error = storage
            .scan_prefix(namespace.clone(), request)
            .expect_err("invalid key encoding must be corruption");
        assert_eq!(error.kind(), StorageErrorKind::Corruption);
        collection.delete_many(doc! {}).run().unwrap();

        collection
            .insert_one(doc! {
                "_id": "aa",
                "value": Bson::Binary(Binary {
                    subtype: BinarySubtype::Generic,
                    bytes: vec![2; MAX_VALUE_BYTES + 1],
                }),
            })
            .run()
            .unwrap();
        let error = storage
            .get(namespace, &Key::new([0xaa]).unwrap())
            .expect_err("oversized stored value must be corruption");
        assert_eq!(error.kind(), StorageErrorKind::Corruption);
    }));

    client.database(&database).drop().run().unwrap();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
#[ignore = "requires OUTBE_TEST_MONGODB_URI"]
fn other_mongo_failures_are_backend_errors() {
    let uri = mongo_uri();
    let database = isolated_database_name("other_mongo_failures_are_backend_errors");
    let client = Client::with_uri_str(&uri).unwrap();
    client.database(&database).drop().run().unwrap();
    let namespace = Namespace::new("rejected_records").unwrap();
    client
        .database(&database)
        .create_collection(namespace.as_str())
        .validator(doc! { "$expr": false })
        .run()
        .unwrap();
    let storage = MongoStorage::connect(MongoStorageConfig {
        uri,
        database: database.clone(),
    })
    .unwrap();

    let error = storage
        .put(
            namespace,
            &Key::new(b"key".to_vec()).unwrap(),
            &Value::new(b"value".to_vec()).unwrap(),
        )
        .expect_err("collection validator must reject the write");
    assert_eq!(error.kind(), StorageErrorKind::Backend);
    client.database(&database).drop().run().unwrap();
}

fn run_isolated(test_name: &str, test: fn(StorageReaderHandle, StorageWriterHandle)) {
    let uri = mongo_uri();
    let database = isolated_database_name(test_name);
    let cleanup_client = Client::with_uri_str(&uri).unwrap();
    cleanup_client.database(&database).drop().run().unwrap();
    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri,
            database: database.clone(),
        })
        .unwrap(),
    );
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| test(reader, writer)));
    cleanup_client.database(&database).drop().run().unwrap();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn mongo_uri() -> String {
    std::env::var("OUTBE_TEST_MONGODB_URI")
        .expect("set OUTBE_TEST_MONGODB_URI before running ignored MongoDB tests")
}

fn isolated_database_name(test_name: &str) -> String {
    let compact_name: String = test_name.chars().take(30).collect();
    format!("outbe_storage_{}_{}", std::process::id(), compact_name)
}
