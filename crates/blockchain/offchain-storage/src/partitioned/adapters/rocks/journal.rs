//! Durable prepared operations: recovery never recomputes routing from new state.

use super::super::super::PartitionedOperation;
use super::{PartitionedBatch, RocksPartitionDataSource, StorageScope};
use crate::{
    AtomicWriteOperation, Key, Namespace, StorageError, StorageMetadata, StoredValue, Value,
    MAX_ATOMIC_BATCH_BYTES, MAX_ATOMIC_BATCH_OPERATIONS, MAX_KEY_BYTES, MAX_METADATA_ENTRIES,
    MAX_METADATA_KEY_BYTES, MAX_METADATA_VALUE_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
};

const MAGIC: &[u8] = b"OUTBE-PARTITION-JOURNAL-3\0";
const JOURNAL: &str = "partition-journal.v3";
// Include bounded keys/metadata/envelope overhead, not just body value bytes.
const MAX_JOURNAL_BYTES: usize = MAX_ATOMIC_BATCH_BYTES
    + MAX_ATOMIC_BATCH_OPERATIONS
        * (MAX_KEY_BYTES
            + MAX_METADATA_ENTRIES * (MAX_METADATA_KEY_BYTES + MAX_METADATA_VALUE_BYTES)
            + 512);

#[derive(Serialize, Deserialize)]
struct WireRecord {
    value: Vec<u8>,
    metadata: Option<BTreeMap<String, String>>,
}

#[derive(Serialize, Deserialize)]
struct WireOperation {
    scope: StorageScope,
    namespace: String,
    key: Vec<u8>,
    record: Option<WireRecord>,
}
fn encode(batch: &PartitionedBatch) -> Result<Vec<u8>, StorageError> {
    let mut operations = vec![];
    for op in &batch.operations {
        let (namespace, key, record) = match &op.operation {
            AtomicWriteOperation::Put {
                namespace,
                key,
                record,
            } => (
                namespace,
                key,
                Some(WireRecord {
                    value: record.value.as_bytes().to_vec(),
                    metadata: record.metadata.as_ref().map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.to_owned(), v.to_owned()))
                            .collect()
                    }),
                }),
            ),
            AtomicWriteOperation::Delete { namespace, key } => (namespace, key, None),
        };
        operations.push(WireOperation {
            scope: op.scope.clone(),
            namespace: namespace.as_str().into(),
            key: key.as_bytes().to_vec(),
            record,
        });
    }
    postcard::to_stdvec(&(operations, &batch.retired_scopes)).map_err(StorageError::backend)
}
fn decode(bytes: &[u8]) -> Result<PartitionedBatch, StorageError> {
    let (operations, retired_scopes): (Vec<WireOperation>, Vec<StorageScope>) =
        postcard::from_bytes(bytes)
            .map_err(|_| StorageError::Corruption("malformed partition journal".into()))?;
    if operations.len() > MAX_ATOMIC_BATCH_OPERATIONS {
        return Err(StorageError::Corruption(
            "partition journal operation bound".into(),
        ));
    }
    let mut batch = PartitionedBatch {
        retired_scopes,
        ..Default::default()
    };
    for op in operations {
        let scope = StorageScope::new(&op.scope.domain, op.scope.partition)?;
        let namespace = Namespace::new(op.namespace)?;
        let key = Key::new(op.key)?;
        let operation = match op.record {
            Some(WireRecord { value, metadata }) => AtomicWriteOperation::put_record(
                namespace,
                key,
                StoredValue {
                    value: Value::new(value)?,
                    metadata: metadata.map(StorageMetadata::new).transpose()?,
                },
            ),
            None => AtomicWriteOperation::delete(namespace, key),
        };
        batch
            .operations
            .push(PartitionedOperation { scope, operation });
    }
    batch.validate()?;
    Ok(batch)
}
impl RocksPartitionDataSource {
    fn journal_path(&self) -> std::path::PathBuf {
        self.root.join("system/shared").join(JOURNAL)
    }
    pub(super) fn save_journal(&self, batch: &PartitionedBatch) -> Result<(), StorageError> {
        let bytes = encode(batch)?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(StorageError::InvalidArgument(
                "partition journal size bound".into(),
            ));
        }
        let path = self.journal_path();
        let temporary = path.with_extension("preparing");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(StorageError::unavailable)?;
        file.write_all(MAGIC).map_err(StorageError::unavailable)?;
        file.write_all(&Sha256::digest(&bytes))
            .map_err(StorageError::unavailable)?;
        file.write_all(&bytes).map_err(StorageError::unavailable)?;
        file.sync_all().map_err(StorageError::unavailable)?;
        std::fs::rename(temporary, &path).map_err(StorageError::unavailable)?;
        File::open(path.parent().expect("journal parent"))
            .and_then(|file| file.sync_all())
            .map_err(StorageError::unavailable)
    }
    pub(super) fn clear_journal(&self) -> Result<(), StorageError> {
        let path = self.journal_path();
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::unavailable(error)),
        }
        File::open(path.parent().expect("journal parent"))
            .and_then(|file| file.sync_all())
            .map_err(StorageError::unavailable)
    }
    pub(super) fn load_journal(&self) -> Result<Option<PartitionedBatch>, StorageError> {
        let path = self.journal_path();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StorageError::unavailable(e)),
        };
        if file.metadata().map_err(StorageError::unavailable)?.len()
            > (MAX_JOURNAL_BYTES + MAGIC.len() + 32) as u64
        {
            return Err(StorageError::Corruption(
                "oversized partition journal".into(),
            ));
        }
        let mut bytes = vec![];
        file.take((MAX_JOURNAL_BYTES + MAGIC.len() + 32 + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(StorageError::unavailable)?;
        let bytes = bytes.strip_prefix(MAGIC).ok_or_else(|| {
            StorageError::Corruption("unsupported partition journal version".into())
        })?;
        let (digest, payload) = bytes.split_at_checked(32).ok_or_else(|| {
            StorageError::Corruption("truncated partition journal checksum".into())
        })?;
        if Sha256::digest(payload).as_slice() != digest {
            return Err(StorageError::Corruption(
                "partition journal checksum mismatch".into(),
            ));
        }
        Ok(Some(decode(payload)?))
    }
}
