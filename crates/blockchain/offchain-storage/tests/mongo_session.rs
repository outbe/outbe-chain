use mongodb::{
    bson::{doc, oid::ObjectId},
    sync::Client,
};
use outbe_offchain_storage::{
    MongoStorageConfig, StorageBackend, StorageCloseError, StorageConfig, StorageProvider,
};
use std::{
    thread,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires isolated OUTBE_TEST_MONGODB_URI replica set with enableTestCommands"]
fn failed_mongo_release_is_reported_and_retried_before_completion() {
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI").unwrap();
    let provider = StorageProvider::new(StorageConfig {
        start_block: 1,
        backend: StorageBackend::MongoDb(MongoStorageConfig {
            uri: uri.clone(),
            database: format!("session_{}", ObjectId::new().to_hex()),
        }),
    })
    .unwrap();
    let opened = provider.open_writer().unwrap();
    let completion = opened.ownership.completion();
    let client = Client::with_uri_str(&uri).unwrap();
    client
        .database("admin")
        .run_command(doc! {
            "configureFailPoint": "failCommand", "mode": { "times": 1 },
            "data": { "failCommands": ["delete"], "errorCode": 13 }
        })
        .run()
        .unwrap();
    drop(opened);
    assert!(matches!(
        completion.wait_timeout(Duration::from_secs(5)),
        Err(StorageCloseError::Cleanup(_))
    ));
    let started = Instant::now();
    loop {
        match completion.wait_timeout(Duration::from_millis(50)) {
            Ok(()) => break,
            Err(_) if started.elapsed() < Duration::from_secs(5) => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(error) => panic!("cleanup did not recover: {error}"),
        }
    }
    // Acknowledged completion makes sole-writer ownership available immediately.
    let next = provider.open_writer().unwrap();
    let next_completion = next.ownership.completion();
    drop(next);
    next_completion
        .wait_timeout(Duration::from_secs(5))
        .unwrap();
}
