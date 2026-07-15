use std::fmt;

use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, Document},
    error::{Error as MongoError, ErrorKind as MongoErrorKind},
    options::Collation,
    sync::{Client, Collection},
};

use crate::{
    AtomicWriteBatch, AtomicWriteOperation, Key, Namespace, ScanEntry, ScanPage, ScanRequest,
    StorageError, StorageMetadata, StorageReader, StorageWriter, StoredValue, Value,
    MAX_SCAN_PAGE_VALUE_BYTES,
};

/// Connection settings for the persistent adapter.
#[derive(Clone, Eq, PartialEq)]
pub struct MongoStorageConfig {
    /// MongoDB connection string.
    pub uri: String,
    /// Database containing the namespace collections.
    pub database: String,
}

impl fmt::Debug for MongoStorageConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoStorageConfig")
            .field("uri", &"<redacted>")
            .field("database", &self.database)
            .finish()
    }
}

/// Persistent MongoDB storage adapter.
pub struct MongoStorage {
    client: Client,
    database: Box<str>,
}

impl fmt::Debug for MongoStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MongoStorage")
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

impl MongoStorage {
    /// Connects to the configured database.
    pub fn connect(config: MongoStorageConfig) -> Result<Self, StorageError> {
        let client = Client::with_uri_str(&config.uri).map_err(map_configuration_error)?;
        Ok(Self {
            client,
            database: config.database.into_boxed_str(),
        })
    }

    fn collection(&self, namespace: &Namespace) -> Collection<Document> {
        self.client
            .database(&self.database)
            .collection(namespace.as_str())
    }

    /// Verifies that the server exposes sessions and a transaction-capable topology.
    pub fn verify_transaction_support(&self) -> Result<(), StorageError> {
        self.client
            .start_session()
            .run()
            .map_err(map_operation_error)?;
        let hello = self
            .client
            .database("admin")
            .run_command(doc! { "hello": 1 })
            .run()
            .map_err(map_operation_error)?;
        if !transaction_topology_supported(&hello) {
            return Err(StorageError::invalid_argument(
                "MongoDB projector requires a replica set or sharded transaction-capable topology",
            ));
        }
        Ok(())
    }
}

fn transaction_topology_supported(hello: &Document) -> bool {
    let has_sessions = hello.contains_key("logicalSessionTimeoutMinutes");
    let replica_set = hello.get_str("setName").is_ok();
    let sharded = hello
        .get_str("msg")
        .is_ok_and(|message| message == "isdbgrid");
    has_sessions && (replica_set || sharded)
}

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

impl StorageWriter for MongoStorage {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        batch.validate()?;
        if batch.is_empty() {
            return Ok(());
        }
        let operations = batch
            .operations()
            .iter()
            .map(PreparedMongoOperation::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let mut session = self
            .client
            .start_session()
            .run()
            .map_err(map_operation_error)?;
        session
            .start_transaction()
            .and_run(|session| {
                for operation in &operations {
                    match operation {
                        PreparedMongoOperation::Put {
                            namespace,
                            encoded_key,
                            document,
                        } => {
                            self.collection(namespace)
                                .replace_one(doc! { "_id": encoded_key }, document.clone())
                                .upsert(true)
                                .collation(simple_binary_collation())
                                .session(&mut *session)
                                .run()?;
                        }
                        PreparedMongoOperation::Delete {
                            namespace,
                            encoded_key,
                        } => {
                            self.collection(namespace)
                                .delete_one(doc! { "_id": encoded_key })
                                .collation(simple_binary_collation())
                                .session(&mut *session)
                                .run()?;
                        }
                    }
                }
                Ok(())
            })
            .map_err(map_operation_error)
    }
}

enum PreparedMongoOperation {
    Put {
        namespace: Namespace,
        encoded_key: String,
        document: Document,
    },
    Delete {
        namespace: Namespace,
        encoded_key: String,
    },
}

impl TryFrom<&AtomicWriteOperation> for PreparedMongoOperation {
    type Error = StorageError;

    fn try_from(operation: &AtomicWriteOperation) -> Result<Self, Self::Error> {
        Ok(match operation {
            AtomicWriteOperation::Put {
                namespace,
                key,
                record,
            } => {
                let encoded_key = hex::encode(key.as_bytes());
                Self::Put {
                    namespace: namespace.clone(),
                    document: encode_document(&encoded_key, record),
                    encoded_key,
                }
            }
            AtomicWriteOperation::Delete { namespace, key } => Self::Delete {
                namespace: namespace.clone(),
                encoded_key: hex::encode(key.as_bytes()),
            },
        })
    }
}

fn encode_document(encoded_key: &str, record: &StoredValue) -> Document {
    let mut document = doc! {
        "_id": encoded_key,
        "value": Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: record.value.as_bytes().to_vec(),
        }),
    };
    if let Some(metadata) = &record.metadata {
        let projection: Document = metadata
            .iter()
            .map(|(key, value)| (key.to_owned(), Bson::String(value.to_owned())))
            .collect();
        document.insert("_projection", projection);
    }
    document
}

fn simple_binary_collation() -> Collation {
    Collation::builder().locale("simple").build()
}

fn prefix_filter(request: ScanRequest<'_>) -> Document {
    let mut bounds = Document::new();
    if let Some(after) = request.after() {
        bounds.insert("$gt", hex::encode(after.as_bytes()));
    } else if !request.prefix().is_empty() {
        bounds.insert("$gte", hex::encode(request.prefix()));
    }
    if let Some(upper_bound) = raw_prefix_upper_bound(request.prefix()) {
        bounds.insert("$lt", hex::encode(upper_bound));
    }

    let range = if bounds.is_empty() {
        Document::new()
    } else {
        doc! { "_id": bounds }
    };

    if range.is_empty() {
        range
    } else {
        // MongoDB range comparisons are type-bracketed. Explicitly include
        // non-string identifiers so a damaged document remains visible to the
        // adapter and is classified as corruption instead of disappearing
        // from a prefix scan.
        doc! {
            "$or": [
                range,
                { "_id": { "$not": { "$type": "string" } } },
            ]
        }
    }
}

fn raw_prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper_bound = prefix.to_vec();
    let index = upper_bound.iter().rposition(|byte| *byte != u8::MAX)?;
    upper_bound[index] += 1;
    upper_bound.truncate(index + 1);
    Some(upper_bound)
}

fn decode_document(
    mut document: Document,
    expected_key: Option<&Key>,
) -> Result<ScanEntry, StorageError> {
    if !(2..=3).contains(&document.len()) {
        return Err(StorageError::Corruption(
            "MongoDB storage document must contain _id, value, and optional _projection".to_owned(),
        ));
    }
    let metadata = document
        .remove("_projection")
        .map(decode_metadata)
        .transpose()?;
    let encoded_key = document
        .remove("_id")
        .and_then(|id| match id {
            Bson::String(id) => Some(id),
            _ => None,
        })
        .ok_or_else(|| {
            StorageError::Corruption("MongoDB storage document has a non-string _id".to_owned())
        })?;
    let raw_key = hex::decode(&encoded_key).map_err(|_| {
        StorageError::Corruption("MongoDB storage document has invalid key encoding".to_owned())
    })?;
    if hex::encode(&raw_key) != encoded_key {
        return Err(StorageError::Corruption(
            "MongoDB storage document key is not canonical lowercase hex".to_owned(),
        ));
    }
    let key = Key::new(raw_key).map_err(|_| {
        StorageError::Corruption("MongoDB storage document contains an invalid key".to_owned())
    })?;
    if expected_key.is_some_and(|expected| expected != &key) {
        return Err(StorageError::Corruption(
            "MongoDB storage document key does not match its lookup key".to_owned(),
        ));
    }

    let binary = document
        .remove("value")
        .and_then(|value| match value {
            Bson::Binary(binary) => Some(binary),
            _ => None,
        })
        .ok_or_else(|| {
            StorageError::Corruption("MongoDB storage document has a non-binary value".to_owned())
        })?;
    if binary.subtype != BinarySubtype::Generic {
        return Err(StorageError::Corruption(
            "MongoDB storage document uses an unexpected binary subtype".to_owned(),
        ));
    }
    let value = Value::new(binary.bytes).map_err(|_| {
        StorageError::Corruption("MongoDB storage document contains an oversized value".to_owned())
    })?;
    if !document.is_empty() {
        return Err(StorageError::Corruption(
            "MongoDB storage document contains unexpected fields".to_owned(),
        ));
    }
    Ok(ScanEntry {
        key,
        value,
        metadata,
    })
}

fn decode_metadata(value: Bson) -> Result<StorageMetadata, StorageError> {
    let Bson::Document(document) = value else {
        return Err(StorageError::Corruption(
            "MongoDB _projection field is not a document".to_owned(),
        ));
    };
    let mut entries = std::collections::BTreeMap::new();
    for (key, value) in document {
        let Bson::String(value) = value else {
            return Err(StorageError::Corruption(
                "MongoDB _projection values must be strings".to_owned(),
            ));
        };
        entries.insert(key, value);
    }
    StorageMetadata::new(entries).map_err(|error| {
        StorageError::Corruption(format!("invalid MongoDB _projection metadata: {error}"))
    })
}

fn map_configuration_error(error: MongoError) -> StorageError {
    match error.kind.as_ref() {
        MongoErrorKind::InvalidArgument { .. } => StorageError::InvalidArgument(error.to_string()),
        _ => map_operation_error(error),
    }
}

fn map_operation_error(error: MongoError) -> StorageError {
    match error.kind.as_ref() {
        MongoErrorKind::DnsResolve { .. }
        | MongoErrorKind::Io(_)
        | MongoErrorKind::ConnectionPoolCleared { .. }
        | MongoErrorKind::ServerSelection { .. } => StorageError::unavailable(error),
        _ => StorageError::backend(error),
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;

    use super::{transaction_topology_supported, MongoStorageConfig};

    #[test]
    fn topology_capability_rejects_standalone_and_accepts_supported_deployments() {
        assert!(!transaction_topology_supported(&doc! {
            "isWritablePrimary": true,
            "logicalSessionTimeoutMinutes": 30,
        }));
        assert!(transaction_topology_supported(&doc! {
            "setName": "rs0",
            "logicalSessionTimeoutMinutes": 30,
        }));
        assert!(transaction_topology_supported(&doc! {
            "msg": "isdbgrid",
            "logicalSessionTimeoutMinutes": 30,
        }));
        assert!(!transaction_topology_supported(&doc! {
            "setName": "rs0",
        }));
    }

    #[test]
    fn configuration_debug_redacts_mongodb_credentials() {
        let config = MongoStorageConfig {
            uri: "mongodb://user:secret@localhost:27017".to_owned(),
            database: "projection".to_owned(),
        };

        let debug = format!("{config:?}");
        assert!(!debug.contains("user:secret"));
        assert!(debug.contains("uri: \"<redacted>\""));
        assert!(debug.contains("database: \"projection\""));
    }
}
