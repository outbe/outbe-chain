use outbe_offchain_storage::partitioned::{
    adapters::MemoryPartitionDataSource, routing::OwnerPrefixRouting,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, PartitionDataSource,
    PartitionReadSource, PartitionedBatch, PartitionedStorage, ScanPage, ScanRequest, StorageError,
    StorageReader, StorageReaderHandle, StorageScope, StorageWriter, StoredValue, Value,
};
use std::sync::{Arc, Mutex};

pub struct Fixture {
    pub storage: PartitionedStorage,
    pub scans: Arc<Mutex<Vec<usize>>>,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_source(Arc::new(MemoryPartitionDataSource::new()))
    }

    pub fn with_source(inner: Arc<dyn PartitionDataSource>) -> Self {
        Self::build(inner, None)
    }

    pub fn with_fault(fault: fn(&mut ScanPage)) -> Self {
        Self::build(Arc::new(MemoryPartitionDataSource::new()), Some(fault))
    }

    fn build(inner: Arc<dyn PartitionDataSource>, fault: Option<fn(&mut ScanPage)>) -> Self {
        let scans = Arc::new(Mutex::new(Vec::new()));
        let source = Arc::new(CountingSource {
            inner,
            scans: scans.clone(),
            fault,
        });
        Self {
            storage: PartitionedStorage::new(
                source,
                Arc::new(OwnerPrefixRouting::new("entity", "owners", 32).unwrap()),
            ),
            scans,
        }
    }

    pub fn put(&self, shard: u32, number: u32, bytes: usize) {
        self.storage
            .apply_atomic(&AtomicWriteBatch::from_operations(vec![
                AtomicWriteOperation::put(
                    namespace().with_scope(scope(shard)),
                    key(number),
                    Value::new(vec![42; bytes]).unwrap(),
                ),
            ]))
            .unwrap();
    }
}

pub fn namespace() -> Namespace {
    Namespace::new("records").unwrap()
}
pub fn scope(shard: u32) -> StorageScope {
    StorageScope::numbered("entity", "owners", shard).unwrap()
}
pub fn key(number: u32) -> Key {
    Key::new(number.to_be_bytes()).unwrap()
}

struct CountingSource {
    inner: Arc<dyn PartitionDataSource>,
    scans: Arc<Mutex<Vec<usize>>>,
    fault: Option<fn(&mut ScanPage)>,
}
impl PartitionReadSource for CountingSource {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        Ok(self.inner.open_reader(scope)?.map(|inner| {
            Arc::new(CountingReader {
                inner,
                scans: self.scans.clone(),
                fault: self.fault,
            }) as StorageReaderHandle
        }))
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        self.inner.list_scopes(domain)
    }
}
impl PartitionDataSource for CountingSource {
    fn commit(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        self.inner.commit(batch)
    }
    fn verify_write_capability(&self) -> Result<(), StorageError> {
        self.inner.verify_write_capability()
    }
}
struct CountingReader {
    inner: StorageReaderHandle,
    scans: Arc<Mutex<Vec<usize>>>,
    fault: Option<fn(&mut ScanPage)>,
}
impl StorageReader for CountingReader {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        self.inner.get_record(namespace, key)
    }
    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        self.scans.lock().unwrap().push(request.limit());
        let mut page = self.inner.scan_prefix(namespace, request)?;
        if let Some(fault) = self.fault {
            fault(&mut page);
        }
        Ok(page)
    }
}
