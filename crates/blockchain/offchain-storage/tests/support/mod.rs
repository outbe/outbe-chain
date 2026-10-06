use outbe_offchain_storage::partitioned::adapters::MemoryPartitionDataSource;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, PartitionReadSource, PartitionRouting,
    PartitionedStorage, ReadLocation, ScanRequest, StorageError, StorageScope, StorageWriter,
    Value,
};
use std::sync::Arc;

pub struct TestRouting;
impl PartitionRouting for TestRouting {
    fn point(
        &self,
        namespace: &Namespace,
        key: &Key,
        source: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        let location = match namespace.as_str() {
            "nods" => {
                let Some(reader) = source.open_reader(&StorageScope::shared("nod")?)? else {
                    return Ok(None);
                };
                let Some(record) = reader.get(Namespace::new("nod_locations")?, key)? else {
                    return Ok(None);
                };
                let [shard] = record.as_bytes() else {
                    return Err(StorageError::Corruption("malformed location".into()));
                };
                if *shard >= 32 {
                    return Err(StorageError::Corruption("invalid shard".into()));
                }
                return Ok(Some(ReadLocation {
                    scope: StorageScope::numbered("nod", "nod-shards", u32::from(*shard))?,
                    require_present: true,
                }));
            }
            "nods_by_owner" => {
                StorageScope::numbered("nod", "nod-shards", u32::from(key.as_bytes()[19] & 31))?
            }
            _ => StorageScope::shared("nod")?,
        };
        Ok(Some(ReadLocation {
            scope: location,
            require_present: false,
        }))
    }
    fn scan(
        &self,
        namespace: &Namespace,
        request: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        if namespace.as_str() == "nods_by_owner" && request.prefix().len() >= 20 {
            return Ok(vec![StorageScope::numbered(
                "nod",
                "nod-shards",
                u32::from(request.prefix()[19] & 31),
            )?]);
        }
        Ok(source
            .list_scopes("nod")?
            .into_iter()
            .filter(|scope| !matches!(scope.partition, outbe_offchain_storage::PartitionId::Shared))
            .collect())
    }
}

pub fn storage() -> PartitionedStorage {
    PartitionedStorage::new(
        Arc::new(MemoryPartitionDataSource::new()),
        Arc::new(TestRouting),
    )
}

pub fn id(day: u32, digest: u8) -> Key {
    let mut bytes = day.to_be_bytes().to_vec();
    bytes.extend([digest; 32]);
    Key::new(bytes).unwrap()
}

pub fn store_nod(storage: &PartitionedStorage, id: &Key, owner: [u8; 20]) {
    let scope = StorageScope::numbered("nod", "nod-shards", u32::from(owner[19] & 31)).unwrap();
    let mut owner_key = owner.to_vec();
    owner_key.extend(id.as_bytes());
    storage
        .apply_atomic(&AtomicWriteBatch::from_operations(vec![
            AtomicWriteOperation::put(
                Namespace::new("nods").unwrap().with_scope(scope),
                id.clone(),
                Value::new(vec![7]).unwrap(),
            ),
            AtomicWriteOperation::put(
                Namespace::new("nods_by_owner").unwrap(),
                Key::new(owner_key).unwrap(),
                Value::new(vec![]).unwrap(),
            ),
            AtomicWriteOperation::put(
                Namespace::new("nod_locations")
                    .unwrap()
                    .with_scope(StorageScope::shared("nod").unwrap()),
                id.clone(),
                Value::new(vec![owner[19] & 31]).unwrap(),
            ),
        ]))
        .unwrap();
}
