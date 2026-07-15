//! Pure off-chain mutation planning for Nod item and bucket projection.

use alloy_primitives::{B256, U256};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::{
    repository::{
        bucket_storage_key, encode_bucket, encode_item, item_key, namespace, owner_index_key,
        NODS_BY_OWNER_NAMESPACE, NODS_NAMESPACE, NOD_BUCKETS_NAMESPACE,
    },
    NodBucketState, NodItemState, NodRepositoryError,
};

/// Code-defined namespaces owned by the Nod repository.
pub const NOD_PROJECTION_NAMESPACES: [&str; 3] = [
    NODS_NAMESPACE,
    NOD_BUCKETS_NAMESPACE,
    NODS_BY_OWNER_NAMESPACE,
];

/// One complete Nod item projection mutation.
pub enum NodItemMutation<'a> {
    /// Store a complete item and its optional primary metadata.
    Store {
        /// Indexed event identity.
        nod_id: U256,
        /// Complete post-mutation body.
        body: &'a NodItemState,
        /// Metadata attached only to the primary body.
        metadata: Option<StorageMetadata>,
    },
    /// Delete one item identity and its index derivable from the old body.
    Delete {
        /// Indexed event identity.
        nod_id: U256,
    },
}

/// One complete Nod bucket projection mutation.
pub enum NodBucketMutation<'a> {
    /// Store a complete bucket and its optional primary metadata.
    Store {
        /// Indexed event identity.
        bucket_key: B256,
        /// Complete post-mutation body.
        body: &'a NodBucketState,
        /// Metadata attached only to the primary body.
        metadata: Option<StorageMetadata>,
    },
    /// Delete one bucket identity.
    Delete {
        /// Indexed event identity.
        bucket_key: B256,
    },
}

/// Plans all Nod item primary/index mutations without storage access.
pub fn plan_nod_item_mutation(
    old: Option<&NodItemState>,
    mutation: NodItemMutation<'_>,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let nod_id = match &mutation {
        NodItemMutation::Store { nod_id, .. } | NodItemMutation::Delete { nod_id } => *nod_id,
    };
    if let Some(old) = old {
        validate_item_identity(nod_id, old)?;
    }

    let mut batch = AtomicWriteBatch::new();
    match mutation {
        NodItemMutation::Store {
            nod_id,
            body,
            metadata,
        } => {
            validate_item_identity(nod_id, body)?;
            let primary_record = match metadata {
                Some(metadata) => StoredValue::with_metadata(encode_item(body)?, metadata),
                None => StoredValue::plain(encode_item(body)?),
            };
            batch.push(AtomicWriteOperation::put_record(
                namespace(NODS_NAMESPACE)?,
                item_key(nod_id)?,
                primary_record,
            ));
            batch.push(AtomicWriteOperation::put(
                namespace(NODS_BY_OWNER_NAMESPACE)?,
                owner_index_key(body.owner, nod_id)?,
                Value::new(Vec::new())?,
            ));
            if let Some(old) = old {
                if old.owner != body.owner {
                    batch.push(AtomicWriteOperation::delete(
                        namespace(NODS_BY_OWNER_NAMESPACE)?,
                        owner_index_key(old.owner, nod_id)?,
                    ));
                }
            }
        }
        NodItemMutation::Delete { nod_id } => {
            if let Some(old) = old {
                batch.push(AtomicWriteOperation::delete(
                    namespace(NODS_BY_OWNER_NAMESPACE)?,
                    owner_index_key(old.owner, nod_id)?,
                ));
            }
            batch.push(AtomicWriteOperation::delete(
                namespace(NODS_NAMESPACE)?,
                item_key(nod_id)?,
            ));
        }
    }
    Ok(batch)
}

/// Plans one Nod bucket primary mutation without storage access.
pub fn plan_nod_bucket_mutation(
    old: Option<&NodBucketState>,
    mutation: NodBucketMutation<'_>,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let bucket_key = match &mutation {
        NodBucketMutation::Store { bucket_key, .. } | NodBucketMutation::Delete { bucket_key } => {
            *bucket_key
        }
    };
    if let Some(old) = old {
        validate_bucket_identity(bucket_key, old)?;
    }

    let mut batch = AtomicWriteBatch::new();
    match mutation {
        NodBucketMutation::Store {
            bucket_key,
            body,
            metadata,
        } => {
            validate_bucket_identity(bucket_key, body)?;
            let primary_record = match metadata {
                Some(metadata) => StoredValue::with_metadata(encode_bucket(body)?, metadata),
                None => StoredValue::plain(encode_bucket(body)?),
            };
            batch.push(AtomicWriteOperation::put_record(
                namespace(NOD_BUCKETS_NAMESPACE)?,
                bucket_storage_key(bucket_key)?,
                primary_record,
            ));
        }
        NodBucketMutation::Delete { bucket_key } => {
            batch.push(AtomicWriteOperation::delete(
                namespace(NOD_BUCKETS_NAMESPACE)?,
                bucket_storage_key(bucket_key)?,
            ));
        }
    }
    Ok(batch)
}

fn validate_item_identity(expected: U256, body: &NodItemState) -> Result<(), NodRepositoryError> {
    if body.nod_id != expected {
        return Err(NodRepositoryError::PrimaryKeyBodyMismatch {
            expected,
            actual: body.nod_id,
        });
    }
    Ok(())
}

fn validate_bucket_identity(
    expected: B256,
    body: &NodBucketState,
) -> Result<(), NodRepositoryError> {
    if body.bucket_key != expected {
        return Err(NodRepositoryError::BucketKeyBodyMismatch {
            expected,
            actual: body.bucket_key,
        });
    }
    Ok(())
}
