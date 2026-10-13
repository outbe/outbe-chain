//! Pure off-chain mutation planning for gem item and bucket projection.

use outbe_offchain_storage::body_projection::{BodyKind, OwnedBody};

use outbe_compressed_entities::WwdEntityId;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::errors::GemRepositoryError;
use crate::repository::{
    bucket_storage_key, decode_bucket, decode_item, item_key, namespace, owner_index_key,
    GEMS_BY_OWNER_NAMESPACE, GEMS_NAMESPACE, GEM_BUCKETS_NAMESPACE,
};
use crate::schema::{GemBucketState, GemItemState};

/// Code-defined namespaces owned by the gem repository.
pub const GEM_PROJECTION_NAMESPACES: [&str; 3] = [
    GEMS_NAMESPACE,
    GEM_BUCKETS_NAMESPACE,
    GEMS_BY_OWNER_NAMESPACE,
];

outbe_offchain_storage::body_projection_session!(
    GemProjectionSession,
    session_from_records,
    WwdEntityId,
    GemItemState,
    GemBucketState,
    GemRepositoryError,
    ITEMS,
    BUCKETS
);

const ITEMS: BodyKind<WwdEntityId, GemItemState, GemRepositoryError> = BodyKind {
    untracked: |entity, identity| GemRepositoryError::UntrackedProjectionIdentity {
        entity,
        identity,
    },
    entity: "Gem item",
    decode: decode_item,
    validate: validate_item_identity,
    plan_store: plan_item_store,
    plan_delete: plan_item_delete,
};

const BUCKETS: BodyKind<WwdEntityId, GemBucketState, GemRepositoryError> = BodyKind {
    untracked: |entity, identity| GemRepositoryError::UntrackedProjectionIdentity {
        entity,
        identity,
    },
    entity: "Gem bucket",
    decode: decode_bucket,
    validate: validate_bucket_identity,
    plan_store: plan_bucket_store,
    plan_delete: plan_bucket_delete,
};

const OWNED_ITEM: OwnedBody<WwdEntityId, GemItemState, GemRepositoryError> = OwnedBody {
    decode: decode_item,
    primary: |id| {
        Ok((
            namespace(GEMS_NAMESPACE)?.with_scope(crate::partitioning::item_scope(id)?),
            item_key(id)?,
        ))
    },
    owner_key: |body, id| owner_index_key(body.owner, id),
    owner_namespace: || namespace(GEMS_BY_OWNER_NAMESPACE),
};

/// Plans one gem item primary and owner-index store without storage access.
fn plan_item_store(
    old: Option<&GemItemState>,
    gem_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    OWNED_ITEM.store(old, gem_id, primary_record(stored_body, metadata))
}

/// Plans one gem item delete. The owner index comes from the old body.
fn plan_item_delete(
    old: Option<&GemItemState>,
    gem_id: WwdEntityId,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    OWNED_ITEM.delete(old, gem_id)
}

/// Plans one gem bucket primary store. The stored bytes must carry this bucket identity.
fn plan_bucket_store(
    _old: Option<&GemBucketState>,
    bucket_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    Ok(AtomicWriteBatch::from_operations(vec![
        AtomicWriteOperation::put_record(
            namespace(GEM_BUCKETS_NAMESPACE)?,
            bucket_storage_key(bucket_id)?,
            primary_record(stored_body, metadata),
        ),
    ]))
}

/// Plans one gem bucket primary delete.
fn plan_bucket_delete(
    _old: Option<&GemBucketState>,
    bucket_id: WwdEntityId,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    batch.push(AtomicWriteOperation::delete(
        namespace(GEM_BUCKETS_NAMESPACE)?,
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
    body: &GemItemState,
) -> Result<(), GemRepositoryError> {
    if body.gem_id != expected {
        return Err(GemRepositoryError::PrimaryKeyBodyMismatch {
            expected,
            actual: body.gem_id,
        });
    }
    Ok(())
}

fn validate_bucket_identity(
    expected: WwdEntityId,
    body: &GemBucketState,
) -> Result<(), GemRepositoryError> {
    let actual = crate::repository::canonical_bucket_id(body);
    if actual != expected {
        return Err(GemRepositoryError::BucketIdBodyMismatch { expected, actual });
    }
    Ok(())
}
