use std::collections::BTreeMap;

use parking_lot::RwLock;

use crate::{
    Key, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError, StorageReader, StorageWriter,
    Value, MAX_SCAN_PAGE_VALUE_BYTES,
};

/// Instance-owned in-memory storage adapter.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    records: RwLock<BTreeMap<Namespace, BTreeMap<Key, Value>>>,
}

impl MemoryStorage {
    /// Creates an empty, isolated adapter instance.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl StorageReader for MemoryStorage {
    fn get(&self, namespace: Namespace, key: &Key) -> Result<Option<Value>, StorageError> {
        Ok(self
            .records
            .read()
            .get(&namespace)
            .and_then(|records| records.get(key))
            .cloned())
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        request.validate()?;
        let records = self.records.read();
        let Some(records) = records.get(&namespace) else {
            return Ok(ScanPage::default());
        };

        let mut entries = Vec::new();
        let mut value_bytes = 0_usize;
        let mut has_more = false;
        for (key, value) in records {
            if !key.as_bytes().starts_with(request.prefix()) {
                continue;
            }
            if request.after().is_some_and(|after| key <= after) {
                continue;
            }
            if entries.len() == request.limit()
                || value_bytes + value.as_bytes().len() > MAX_SCAN_PAGE_VALUE_BYTES
            {
                has_more = true;
                break;
            }
            value_bytes += value.as_bytes().len();
            entries.push(ScanEntry {
                key: key.clone(),
                value: value.clone(),
            });
        }

        let next_after = has_more.then(|| {
            entries
                .last()
                .expect("a valid value always fits in an empty scan page")
                .key
                .clone()
        });
        Ok(ScanPage {
            entries,
            next_after,
        })
    }
}

impl StorageWriter for MemoryStorage {
    fn put(&self, namespace: Namespace, key: &Key, value: &Value) -> Result<(), StorageError> {
        self.records
            .write()
            .entry(namespace)
            .or_default()
            .insert(key.clone(), value.clone());
        Ok(())
    }

    fn delete(&self, namespace: Namespace, key: &Key) -> Result<(), StorageError> {
        if let Some(records) = self.records.write().get_mut(&namespace) {
            records.remove(key);
        }
        Ok(())
    }
}
