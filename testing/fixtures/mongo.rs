use mongodb::sync::Client;
use outbe_offchain_storage::{
    MongoStorage, MongoStorageConfig, StorageReaderHandle, StorageWriterHandle,
};
use std::{panic::AssertUnwindSafe, sync::Arc};

pub fn run_isolated_mongo(test_name: &str, test: fn(StorageReaderHandle, StorageWriterHandle)) {
    let uri = std::env::var("OUTBE_TEST_MONGODB_URI")
        .expect("set OUTBE_TEST_MONGODB_URI before running ignored MongoDB tests");
    let database = format!("outbe_{}_{}_{}", test_name, std::process::id(), 1);
    let client = Client::with_uri_str(&uri).unwrap();
    client.database(&database).drop().run().unwrap();
    let storage = Arc::new(
        MongoStorage::connect(MongoStorageConfig {
            uri,
            database: database.clone(),
        })
        .unwrap(),
    );
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        test(storage.clone(), storage);
    }));
    client.database(&database).drop().run().unwrap();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
