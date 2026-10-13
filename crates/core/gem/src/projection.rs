//! Pure off-chain mutation planning for gem item and bucket projection.

use std::collections::BTreeMap;

use outbe_compressed_entities::WwdEntityId;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::errors::GemRepositoryError;
use crate::repository::{
    bucket_storage_key, decode_bucket, decode_item, item_key, namespace, owner_index_key,
    GemBucketRecordWithMetadata, GemItemRecordWithMetadata, GEMS_BY_OWNER_NAMESPACE,
    GEMS_NAMESPACE, GEM_BUCKETS_NAMESPACE,
};
use crate::schema::{GemBucketState, GemItemState};

/// Code-defined namespaces owned by the gem repository.
pub const GEM_PROJECTION_NAMESPACES: [&str; 3] = [
    GEMS_NAMESPACE,
    GEM_BUCKETS_NAMESPACE,
    GEMS_BY_OWNER_NAMESPACE,
];

/// Repository-owned prior state plus an in-block overlay for gem projection.
///
/// Callers can mutate only identities loaded through
/// [`crate::GemRepositoryReader::projection_session`]. They cannot supply or omit arbitrary
/// semantic prior item or bucket bodies.
pub struct GemProjectionSession {
    items: BTreeMap<WwdEntityId, Option<GemItemRecordWithMetadata>>,
    buckets: BTreeMap<WwdEntityId, Option<GemBucketRecordWithMetadata>>,
}

type Records<T> = BTreeMap<WwdEntityId, Option<(T, Option<StorageMetadata>)>>;

/// How the session decodes, validates and plans one kind of body.
struct BodyKind<T> {
    entity: &'static str,
    decode: fn(WwdEntityId, &[u8]) -> Result<T, GemRepositoryError>,
    validate: fn(WwdEntityId, &T) -> Result<(), GemRepositoryError>,
    plan_store: StorePlanner<T>,
    plan_delete: fn(Option<&T>, WwdEntityId) -> Result<AtomicWriteBatch, GemRepositoryError>,
}

type StorePlanner<T> = fn(
    Option<&T>,
    WwdEntityId,
    Value,
    Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, GemRepositoryError>;

const ITEMS: BodyKind<GemItemState> = BodyKind {
    entity: "Gem item",
    decode: decode_item,
    validate: validate_item_identity,
    plan_store: plan_item_store,
    plan_delete: plan_item_delete,
};

const BUCKETS: BodyKind<GemBucketState> = BodyKind {
    entity: "Gem bucket",
    decode: decode_bucket,
    validate: validate_bucket_identity,
    plan_store: plan_bucket_store,
    plan_delete: plan_bucket_delete,
};

pub(crate) fn session_from_records(
    gem_ids: &[WwdEntityId],
    items: Vec<Option<GemItemRecordWithMetadata>>,
    bucket_ids: &[WwdEntityId],
    buckets: Vec<Option<GemBucketRecordWithMetadata>>,
) -> GemProjectionSession {
    GemProjectionSession {
        items: gem_ids.iter().copied().zip(items).collect(),
        buckets: bucket_ids.iter().copied().zip(buckets).collect(),
    }
}

impl GemProjectionSession {
    /// Returns the current item body from the repository snapshot or in-block overlay.
    pub fn current_item(
        &self,
        gem_id: WwdEntityId,
    ) -> Result<Option<&GemItemState>, GemRepositoryError> {
        Ok(self
            .current_item_with_metadata(gem_id)?
            .map(|(body, _)| body))
    }

    /// Returns the current item and provenance without exposing a constructible prior snapshot.
    pub fn current_item_with_metadata(
        &self,
        gem_id: WwdEntityId,
    ) -> Result<Option<(&GemItemState, Option<&StorageMetadata>)>, GemRepositoryError> {
        tracked(&self.items, ITEMS.entity, gem_id)
    }

    /// Returns the current bucket body from the repository snapshot or in-block overlay.
    pub fn current_bucket(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<&GemBucketState>, GemRepositoryError> {
        Ok(self
            .current_bucket_with_metadata(bucket_id)?
            .map(|(body, _)| body))
    }

    /// Returns the current bucket and provenance without exposing a constructible prior snapshot.
    pub fn current_bucket_with_metadata(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<(&GemBucketState, Option<&StorageMetadata>)>, GemRepositoryError> {
        tracked(&self.buckets, BUCKETS.entity, bucket_id)
    }

    /// Plans one canonical item store and advances the overlay after full validation.
    pub fn store_item(
        &mut self,
        gem_id: WwdEntityId,
        stored_body: Value,
        metadata: Option<StorageMetadata>,
    ) -> Result<AtomicWriteBatch, GemRepositoryError> {
        store(&mut self.items, &ITEMS, gem_id, stored_body, metadata)
    }

    /// Plans one item delete from the owned prior snapshot and advances overlay to absence.
    pub fn delete_item(
        &mut self,
        gem_id: WwdEntityId,
    ) -> Result<AtomicWriteBatch, GemRepositoryError> {
        delete(&mut self.items, &ITEMS, gem_id)
    }

    /// Plans one canonical bucket store and advances the overlay after full validation.
    pub fn store_bucket(
        &mut self,
        bucket_id: WwdEntityId,
        stored_body: Value,
        metadata: Option<StorageMetadata>,
    ) -> Result<AtomicWriteBatch, GemRepositoryError> {
        store(
            &mut self.buckets,
            &BUCKETS,
            bucket_id,
            stored_body,
            metadata,
        )
    }

    /// Plans one bucket delete from the owned prior snapshot and advances overlay to absence.
    pub fn delete_bucket(
        &mut self,
        bucket_id: WwdEntityId,
    ) -> Result<AtomicWriteBatch, GemRepositoryError> {
        delete(&mut self.buckets, &BUCKETS, bucket_id)
    }
}

/// Looks up an identity the session loaded, with its provenance.
fn tracked<'a, T>(
    records: &'a Records<T>,
    entity: &'static str,
    identity: WwdEntityId,
) -> Result<Option<(&'a T, Option<&'a StorageMetadata>)>, GemRepositoryError> {
    match records
        .get(&identity)
        .ok_or(GemRepositoryError::UntrackedProjectionIdentity { entity, identity })?
    {
        Some((body, metadata)) => Ok(Some((body, metadata.as_ref()))),
        None => Ok(None),
    }
}

/// The identity's current body, checked against the identity.
fn validated_current<'a, T>(
    records: &'a Records<T>,
    kind: &BodyKind<T>,
    identity: WwdEntityId,
) -> Result<Option<&'a T>, GemRepositoryError> {
    let old = tracked(records, kind.entity, identity)?.map(|(body, _)| body);
    if let Some(old) = old {
        (kind.validate)(identity, old)?;
    }
    Ok(old)
}

/// Plans one store from the exact canonical StoredBody bytes, then advances the overlay.
fn store<T>(
    records: &mut Records<T>,
    kind: &BodyKind<T>,
    identity: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    let body = (kind.decode)(identity, stored_body.as_bytes())?;
    let old = validated_current(records, kind, identity)?;
    let batch = (kind.plan_store)(old, identity, stored_body, metadata.clone())?;
    records.insert(identity, Some((body, metadata)));
    Ok(batch)
}

/// Plans one delete from the prior snapshot, then advances the overlay to absence.
fn delete<T>(
    records: &mut Records<T>,
    kind: &BodyKind<T>,
    identity: WwdEntityId,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    let old = validated_current(records, kind, identity)?;
    let batch = (kind.plan_delete)(old, identity)?;
    records.insert(identity, None);
    Ok(batch)
}

/// Plans one gem item primary and owner-index store without storage access.
fn plan_item_store(
    old: Option<&GemItemState>,
    gem_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    let body = decode_item(gem_id, stored_body.as_bytes())?;
    let primary_record = primary_record(stored_body, metadata);
    let scope = crate::partitioning::item_scope(gem_id)?;
    batch.push(AtomicWriteOperation::put_record(
        namespace(GEMS_NAMESPACE)?.with_scope(scope),
        item_key(gem_id)?,
        primary_record,
    ));
    batch.push(AtomicWriteOperation::put(
        namespace(GEMS_BY_OWNER_NAMESPACE)?,
        owner_index_key(body.owner, gem_id)?,
        Value::new(Vec::new())?,
    ));
    if let Some(old) = old {
        if old.owner != body.owner {
            batch.push(AtomicWriteOperation::delete(
                namespace(GEMS_BY_OWNER_NAMESPACE)?,
                owner_index_key(old.owner, gem_id)?,
            ));
        }
    }
    Ok(batch)
}

/// Plans one gem item delete. The owner index comes from the old body.
fn plan_item_delete(
    old: Option<&GemItemState>,
    gem_id: WwdEntityId,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    if let Some(old) = old {
        batch.push(AtomicWriteOperation::delete(
            namespace(GEMS_BY_OWNER_NAMESPACE)?,
            owner_index_key(old.owner, gem_id)?,
        ));
    }
    batch.push(AtomicWriteOperation::delete(
        namespace(GEMS_NAMESPACE)?.with_scope(crate::partitioning::item_scope(gem_id)?),
        item_key(gem_id)?,
    ));
    Ok(batch)
}

/// Plans one gem bucket primary store. The stored bytes must carry this bucket identity.
fn plan_bucket_store(
    _old: Option<&GemBucketState>,
    bucket_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, GemRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    decode_bucket(bucket_id, stored_body.as_bytes())?;
    batch.push(AtomicWriteOperation::put_record(
        namespace(GEM_BUCKETS_NAMESPACE)?,
        bucket_storage_key(bucket_id)?,
        primary_record(stored_body, metadata),
    ));
    Ok(batch)
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
