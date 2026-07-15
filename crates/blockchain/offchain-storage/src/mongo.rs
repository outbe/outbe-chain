use std::fmt;

use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, Document},
    error::{Error as MongoError, ErrorKind as MongoErrorKind},
    options::Collation,
    sync::{Client, Collection},
};

use crate::{
    Key, Namespace, ScanEntry, ScanPage, ScanRequest, StorageError, StorageReader, StorageWriter,
    Value, MAX_SCAN_PAGE_VALUE_BYTES,
};

/// Connection settings for the persistent adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MongoStorageConfig {
    /// MongoDB connection string.
    pub uri: String,
    /// Database containing the namespace collections.
    pub database: String,
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
}

impl StorageReader for MongoStorage {
    fn get(&self, namespace: Namespace, key: &Key) -> Result<Option<Value>, StorageError> {
        let encoded_key = hex::encode(key.as_bytes());
        self.collection(&namespace)
            .find_one(doc! { "_id": &encoded_key })
            .collation(simple_binary_collation())
            .run()
            .map_err(map_operation_error)?
            .map(|document| decode_document(document, Some(key)).map(|entry| entry.value))
            .transpose()
    }

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError> {
        request.validate()?;
        let filter = prefix_filter(request);
        let internal_limit =
            i64::try_from(request.limit() + 1).expect("the provisional page limit fits in i64");
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
                || value_bytes + entry.value.as_bytes().len() > MAX_SCAN_PAGE_VALUE_BYTES
            {
                has_more = true;
                break;
            }
            value_bytes += entry.value.as_bytes().len();
            entries.push(entry);
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

impl StorageWriter for MongoStorage {
    fn put(&self, namespace: Namespace, key: &Key, value: &Value) -> Result<(), StorageError> {
        let encoded_key = hex::encode(key.as_bytes());
        let document = doc! {
            "_id": &encoded_key,
            "value": Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: value.as_bytes().to_vec(),
            }),
        };
        self.collection(&namespace)
            .replace_one(doc! { "_id": &encoded_key }, document)
            .upsert(true)
            .collation(simple_binary_collation())
            .run()
            .map_err(map_operation_error)?;
        Ok(())
    }

    fn delete(&self, namespace: Namespace, key: &Key) -> Result<(), StorageError> {
        self.collection(&namespace)
            .delete_one(doc! { "_id": hex::encode(key.as_bytes()) })
            .collation(simple_binary_collation())
            .run()
            .map_err(map_operation_error)?;
        Ok(())
    }
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
    if document.len() != 2 {
        return Err(StorageError::Corruption(
            "MongoDB storage document must contain exactly _id and value".to_owned(),
        ));
    }
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
    Ok(ScanEntry { key, value })
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
