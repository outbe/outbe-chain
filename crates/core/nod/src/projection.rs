//! Pure off-chain mutation planning for Nod item and bucket projection.

use outbe_offchain_storage::body_projection::{BodyKind, OwnedBody};

use outbe_compressed_entities::WwdEntityId;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::{
    repository::{
        bucket_storage_key, decode_bucket, decode_item, item_key, namespace, owner_index_key,
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

outbe_offchain_storage::body_projection_session!(
    NodProjectionSession,
    session_from_records,
    WwdEntityId,
    NodItemState,
    NodBucketState,
    NodRepositoryError,
    ITEMS,
    BUCKETS
);

const ITEMS: BodyKind<WwdEntityId, NodItemState, NodRepositoryError> = BodyKind {
    untracked: |entity, identity| NodRepositoryError::UntrackedProjectionIdentity {
        entity,
        identity,
    },
    entity: "Nod item",
    decode: decode_item,
    validate: validate_item_identity,
    plan_store: plan_item_store,
    plan_delete: plan_item_delete,
};

const BUCKETS: BodyKind<WwdEntityId, NodBucketState, NodRepositoryError> = BodyKind {
    untracked: |entity, identity| NodRepositoryError::UntrackedProjectionIdentity {
        entity,
        identity,
    },
    entity: "Nod bucket",
    decode: decode_bucket,
    validate: validate_bucket_identity,
    plan_store: plan_bucket_store,
    plan_delete: plan_bucket_delete,
};

const OWNED_ITEM: OwnedBody<WwdEntityId, NodItemState, NodRepositoryError> = OwnedBody {
    decode: decode_item,
    primary: |id| {
        Ok((
            namespace(NODS_NAMESPACE)?.with_scope(crate::partitioning::item_scope(id)?),
            item_key(id)?,
        ))
    },
    owner_key: |body, id| owner_index_key(body.owner, id),
    owner_namespace: || namespace(NODS_BY_OWNER_NAMESPACE),
};

/// Plans one Nod item primary/index store and derives its indexes from the decoded body.
fn plan_item_store(
    old: Option<&NodItemState>,
    nod_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    OWNED_ITEM.store(old, nod_id, primary_record(stored_body, metadata))
}

/// Plans one Nod item delete, with the index derivable from the old body.
fn plan_item_delete(
    old: Option<&NodItemState>,
    nod_id: WwdEntityId,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    OWNED_ITEM.delete(old, nod_id)
}

/// Plans one Nod bucket primary store without storage access. Decodes the
/// stored body to validate its canonical bucket identity.
fn plan_bucket_store(
    _old: Option<&NodBucketState>,
    bucket_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    Ok(AtomicWriteBatch::from_operations(vec![
        AtomicWriteOperation::put_record(
            namespace(NOD_BUCKETS_NAMESPACE)?,
            bucket_storage_key(bucket_id)?,
            primary_record(stored_body, metadata),
        ),
    ]))
}

/// Plans one Nod bucket primary delete without storage access.
fn plan_bucket_delete(
    _old: Option<&NodBucketState>,
    bucket_id: WwdEntityId,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    batch.push(AtomicWriteOperation::delete(
        namespace(NOD_BUCKETS_NAMESPACE)?,
        bucket_storage_key(bucket_id)?,
    ));
    Ok(batch)
}

/// The primary body record, carrying `metadata` when present.
fn primary_record(stored_body: Value, metadata: Option<StorageMetadata>) -> StoredValue {
    match metadata {
        Some(metadata) => StoredValue::with_metadata(stored_body, metadata),
        None => StoredValue::plain(stored_body),
    }
}

fn validate_item_identity(
    expected: WwdEntityId,
    body: &NodItemState,
) -> Result<(), NodRepositoryError> {
    if body.nod_id != expected {
        return Err(NodRepositoryError::PrimaryKeyBodyMismatch {
            expected,
            actual: body.nod_id,
        });
    }
    Ok(())
}

fn validate_bucket_identity(
    expected: WwdEntityId,
    body: &NodBucketState,
) -> Result<(), NodRepositoryError> {
    let actual = crate::repository::canonical_bucket_id(body);
    if actual != expected {
        return Err(NodRepositoryError::BucketIdBodyMismatch { expected, actual });
    }
    Ok(())
}
