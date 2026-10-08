use outbe_offchain_storage::{
    Key, Namespace, ScanPage, ScanRequest, StorageError, StorageReader, StoredValue,
};

pub struct FailingStorageReader(pub fn() -> StorageError);

impl StorageReader for FailingStorageReader {
    fn get_record(
        &self,
        _namespace: Namespace,
        _key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        Err((self.0)())
    }
    fn scan_prefix(
        &self,
        _namespace: Namespace,
        _request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        Err((self.0)())
    }
}
