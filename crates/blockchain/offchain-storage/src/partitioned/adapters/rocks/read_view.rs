use super::{layout, PartitionReadSource, StorageScope};
use crate::{RocksDbReader, StorageError, StorageReaderHandle};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub struct RocksPartitionReadView {
    readers: BTreeMap<StorageScope, StorageReaderHandle>,
}
impl RocksPartitionReadView {
    pub fn open(root: &Path, scratch: &Path) -> Result<Self, StorageError> {
        layout::reject_legacy(root)?;
        let root = std::fs::canonicalize(root).map_err(StorageError::unavailable)?;
        let resolved_scratch = crate::rocks::resolve_existing_ancestor(scratch)?;
        if root.starts_with(&resolved_scratch) || resolved_scratch.starts_with(&root) {
            return Err(StorageError::invalid_argument(
                "partition primary root and read session scratch overlap",
            ));
        }
        if root.join("system/shared/partition-journal.v3").exists() {
            return Err(StorageError::Corruption(
                "unfinished partition commit; recover the writer before opening an export session"
                    .into(),
            ));
        }
        let mut readers = BTreeMap::new();
        for scope in layout::enumerate(&root)? {
            let reader = RocksDbReader::open(
                &root.join(layout::relative_path(&scope)),
                &scratch.join(layout::relative_path(&scope)),
            )?;
            readers.insert(scope, Arc::new(reader) as StorageReaderHandle);
        }
        if !readers.contains_key(&StorageScope::shared("system")?) {
            return Err(StorageError::Corruption(
                "missing system/shared projection database".into(),
            ));
        }
        Ok(Self { readers })
    }
}
impl PartitionReadSource for RocksPartitionReadView {
    fn open_reader(
        &self,
        scope: &StorageScope,
    ) -> Result<Option<StorageReaderHandle>, StorageError> {
        scope.validate()?;
        Ok(self.readers.get(scope).cloned())
    }
    fn list_scopes(&self, domain: &str) -> Result<Vec<StorageScope>, StorageError> {
        Ok(self
            .readers
            .keys()
            .filter(|s| s.domain == domain)
            .cloned()
            .collect())
    }
}
