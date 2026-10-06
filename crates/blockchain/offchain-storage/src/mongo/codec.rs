//! MongoDB representation and consistency options.
use super::*;

pub(super) fn execution_database_options() -> DatabaseOptions {
    DatabaseOptions::builder()
        .selection_criteria(primary_selection())
        .read_concern(ReadConcern::majority())
        .write_concern(majority_write_concern())
        .build()
}

pub(super) fn primary_selection() -> SelectionCriteria {
    SelectionCriteria::ReadPreference(ReadPreference::Primary)
}

pub(super) fn majority_write_concern() -> WriteConcern {
    WriteConcern::builder()
        .w(Acknowledgment::Majority)
        .w_timeout(EXECUTION_READ_TIMEOUT)
        .build()
}

pub(super) enum PreparedMongoOperation {
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

pub(super) fn encode_document(encoded_key: &str, record: &StoredValue) -> Document {
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

pub(super) fn simple_binary_collation() -> Collation {
    Collation::builder().locale("simple").build()
}

pub(super) fn prefix_filter(request: ScanRequest<'_>) -> Document {
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
        // adapter. The adapter then classifies the document as corruption, and
        // the document does not disappear from a prefix scan.
        doc! {
            "$or": [
                range,
                { "_id": { "$not": { "$type": "string" } } },
            ]
        }
    }
}

pub(super) fn raw_prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper_bound = prefix.to_vec();
    let index = upper_bound.iter().rposition(|byte| *byte != u8::MAX)?;
    upper_bound[index] += 1;
    upper_bound.truncate(index + 1);
    Some(upper_bound)
}

pub(super) fn decode_document(
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

pub(super) fn decode_metadata(value: Bson) -> Result<StorageMetadata, StorageError> {
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

pub(super) fn map_configuration_error(error: MongoError) -> StorageError {
    match error.kind.as_ref() {
        MongoErrorKind::InvalidArgument { .. } => StorageError::InvalidArgument(error.to_string()),
        _ => map_operation_error(error),
    }
}

pub(super) fn map_operation_error(error: MongoError) -> StorageError {
    if error.get_custom::<WriterLeaseLost>().is_some() {
        return StorageError::WriterLeaseLost;
    }
    match error.kind.as_ref() {
        MongoErrorKind::DnsResolve { .. }
        | MongoErrorKind::Io(_)
        | MongoErrorKind::ConnectionPoolCleared { .. }
        | MongoErrorKind::ServerSelection { .. }
        | MongoErrorKind::Write(WriteFailure::WriteConcernError(_)) => {
            StorageError::unavailable(error)
        }
        MongoErrorKind::Command(command)
            if matches!(
                command.code,
                WRITE_CONCERN_FAILED_CODE | MAX_TIME_MS_EXPIRED_CODE
            ) || RETRYABLE_READ_CODES.contains(&command.code) =>
        {
            StorageError::unavailable(error)
        }
        _ => StorageError::backend(error),
    }
}
