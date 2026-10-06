//! Majority-consistent point and ordered scan reads.
use super::*;

impl StorageReader for MongoStorage {
    fn get_record(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<StoredValue>, StorageError> {
        let encoded_key = hex::encode(key.as_bytes());
        self.collection(&namespace)
            .find_one(doc! { "_id": &encoded_key })
            .collation(simple_binary_collation())
            .max_time(EXECUTION_READ_TIMEOUT)
            .run()
            .map_err(map_operation_error)?
            .map(|document| {
                decode_document(document, Some(key)).map(|entry| StoredValue {
                    value: entry.value,
                    metadata: entry.metadata,
                })
            })
            .transpose()
    }

    fn get_records(
        &self,
        namespace: Namespace,
        keys: &[Key],
    ) -> Result<Vec<Option<StoredValue>>, StorageError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let encoded_keys: Vec<_> = keys
            .iter()
            .map(|key| Bson::String(hex::encode(key.as_bytes())))
            .collect();
        let cursor = self
            .collection(&namespace)
            .find(doc! { "_id": { "$in": encoded_keys } })
            .collation(simple_binary_collation())
            .max_time(EXECUTION_READ_TIMEOUT)
            .run()
            .map_err(map_operation_error)?;
        let mut records = std::collections::HashMap::new();
        for result in cursor {
            let entry = decode_document(result.map_err(map_operation_error)?, None)?;
            let record = StoredValue {
                value: entry.value,
                metadata: entry.metadata,
            };
            if records.insert(entry.key, record).is_some() {
                return Err(StorageError::Corruption(
                    "MongoDB returned a duplicate storage key".to_owned(),
                ));
            }
        }
        Ok(keys.iter().map(|key| records.get(key).cloned()).collect())
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        request.validate()?;
        let filter = prefix_filter(request);
        let internal_limit = i64::try_from(request.limit() + 1)
            .map_err(|_| StorageError::invalid_argument("scan limit does not fit i64"))?;
        let cursor = self
            .collection(&namespace)
            .find(filter)
            .sort(doc! { "_id": 1 })
            .collation(simple_binary_collation())
            .limit(internal_limit)
            .max_time(EXECUTION_READ_TIMEOUT)
            .run()
            .map_err(map_operation_error)?;

        let mut entries = Vec::new();
        let mut value_bytes = 0_usize;
        let mut has_more = false;
        for result in cursor {
            let entry = decode_document(result.map_err(map_operation_error)?, None)?;
            if !entry.key.as_bytes().starts_with(request.prefix()) {
                return Err(StorageError::Corruption(
                    "MongoDB prefix query returned an out-of-range key".to_owned(),
                ));
            }
            if entries.len() == request.limit()
                || value_bytes
                    + entry.value.as_bytes().len()
                    + entry
                        .metadata
                        .as_ref()
                        .map_or(0, |metadata| metadata.encoded_len())
                    > MAX_SCAN_PAGE_VALUE_BYTES
            {
                has_more = true;
                break;
            }
            value_bytes += entry.value.as_bytes().len()
                + entry
                    .metadata
                    .as_ref()
                    .map_or(0, |metadata| metadata.encoded_len());
            entries.push(entry);
        }

        let next_after = if has_more {
            Some(
                entries
                    .last()
                    .ok_or_else(|| {
                        StorageError::Corruption(
                            "stored record exceeds the scan page byte bound".to_owned(),
                        )
                    })?
                    .key
                    .clone(),
            )
        } else {
            None
        };
        Ok(ScanPage {
            entries,
            next_after,
        })
    }
}
