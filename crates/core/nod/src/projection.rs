//! Pure off-chain mutation planning for Nod item and bucket projection.

use std::collections::BTreeMap;

use outbe_compressed_entities::WwdEntityId;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::{
    repository::{
        bucket_storage_key, decode_bucket, decode_item, item_key, namespace, owner_index_key,
        NODS_BY_OWNER_NAMESPACE, NODS_NAMESPACE, NOD_BUCKETS_NAMESPACE,
    },
    repository::{NodBucketRecordWithMetadata, NodItemRecordWithMetadata},
    NodBucketState, NodItemState, NodRepositoryError,
};

/// Code-defined namespaces owned by the Nod repository.
pub const NOD_PROJECTION_NAMESPACES: [&str; 4] = [
    NODS_NAMESPACE,
    NOD_BUCKETS_NAMESPACE,
    NODS_BY_OWNER_NAMESPACE,
    crate::partitioning::NOD_LOCATIONS_NAMESPACE,
];

/// Repository-owned prior state plus an in-block overlay for Nod projection.
///
/// Callers can mutate only identities loaded through
/// [`crate::NodRepositoryReader::projection_session`]. They cannot supply or omit arbitrary
/// semantic prior item or bucket bodies.
pub struct NodProjectionSession {
    items: BTreeMap<WwdEntityId, Option<NodItemRecordWithMetadata>>,
    buckets: BTreeMap<WwdEntityId, Option<NodBucketRecordWithMetadata>>,
}

type Records<T> = BTreeMap<WwdEntityId, Option<(T, Option<StorageMetadata>)>>;

/// How the session decodes, validates and plans one kind of body.
struct BodyKind<T> {
    entity: &'static str,
    decode: fn(WwdEntityId, &[u8]) -> Result<T, NodRepositoryError>,
    validate: fn(WwdEntityId, &T) -> Result<(), NodRepositoryError>,
    plan_store: StorePlanner<T>,
    plan_delete: fn(Option<&T>, WwdEntityId) -> Result<AtomicWriteBatch, NodRepositoryError>,
}

type StorePlanner<T> = fn(
    Option<&T>,
    WwdEntityId,
    Value,
    Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, NodRepositoryError>;

const ITEMS: BodyKind<NodItemState> = BodyKind {
    entity: "Nod item",
    decode: decode_item,
    validate: validate_item_identity,
    plan_store: plan_item_store,
    plan_delete: plan_item_delete,
};

const BUCKETS: BodyKind<NodBucketState> = BodyKind {
    entity: "Nod bucket",
    decode: decode_bucket,
    validate: validate_bucket_identity,
    plan_store: plan_bucket_store,
    plan_delete: plan_bucket_delete,
};

impl NodProjectionSession {
    pub(crate) fn from_records(
        nod_ids: &[WwdEntityId],
        items: Vec<Option<NodItemRecordWithMetadata>>,
        bucket_ids: &[WwdEntityId],
        buckets: Vec<Option<NodBucketRecordWithMetadata>>,
    ) -> Self {
        Self {
            items: nod_ids.iter().copied().zip(items).collect(),
            buckets: bucket_ids.iter().copied().zip(buckets).collect(),
        }
    }

    /// Returns the current item body from the repository snapshot or in-block overlay.
    pub fn current_item(
        &self,
        nod_id: WwdEntityId,
    ) -> Result<Option<&NodItemState>, NodRepositoryError> {
        Ok(self
            .current_item_with_metadata(nod_id)?
            .map(|(body, _)| body))
    }

    /// Returns the current item and provenance without exposing a constructible prior snapshot.
    pub fn current_item_with_metadata(
        &self,
        nod_id: WwdEntityId,
    ) -> Result<Option<(&NodItemState, Option<&StorageMetadata>)>, NodRepositoryError> {
        tracked(&self.items, ITEMS.entity, nod_id)
    }

    /// Returns the current bucket body from the repository snapshot or in-block overlay.
    pub fn current_bucket(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<&NodBucketState>, NodRepositoryError> {
        Ok(self
            .current_bucket_with_metadata(bucket_id)?
            .map(|(body, _)| body))
    }

    /// Returns the current bucket and provenance without exposing a constructible prior snapshot.
    pub fn current_bucket_with_metadata(
        &self,
        bucket_id: WwdEntityId,
    ) -> Result<Option<(&NodBucketState, Option<&StorageMetadata>)>, NodRepositoryError> {
        tracked(&self.buckets, BUCKETS.entity, bucket_id)
    }

    /// Plans one canonical item store and advances the overlay after full validation.
    pub fn store_item(
        &mut self,
        nod_id: WwdEntityId,
        stored_body: Value,
        metadata: Option<StorageMetadata>,
    ) -> Result<AtomicWriteBatch, NodRepositoryError> {
        store(&mut self.items, &ITEMS, nod_id, stored_body, metadata)
    }

    /// Plans one item delete from the owned prior snapshot and advances overlay to absence.
    pub fn delete_item(
        &mut self,
        nod_id: WwdEntityId,
    ) -> Result<AtomicWriteBatch, NodRepositoryError> {
        delete(&mut self.items, &ITEMS, nod_id)
    }

    /// Plans one canonical bucket store and advances the overlay after full validation.
    pub fn store_bucket(
        &mut self,
        bucket_id: WwdEntityId,
        stored_body: Value,
        metadata: Option<StorageMetadata>,
    ) -> Result<AtomicWriteBatch, NodRepositoryError> {
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
    ) -> Result<AtomicWriteBatch, NodRepositoryError> {
        delete(&mut self.buckets, &BUCKETS, bucket_id)
    }
}

/// Looks up an identity the session loaded, with its provenance.
fn tracked<'a, T>(
    records: &'a Records<T>,
    entity: &'static str,
    identity: WwdEntityId,
) -> Result<Option<(&'a T, Option<&'a StorageMetadata>)>, NodRepositoryError> {
    match records
        .get(&identity)
        .ok_or(NodRepositoryError::UntrackedProjectionIdentity { entity, identity })?
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
) -> Result<Option<&'a T>, NodRepositoryError> {
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
) -> Result<AtomicWriteBatch, NodRepositoryError> {
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
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let old = validated_current(records, kind, identity)?;
    let batch = (kind.plan_delete)(old, identity)?;
    records.insert(identity, None);
    Ok(batch)
}

/// Plans one Nod item primary/index store without storage access. Decodes the
/// stored body and derives every semantic index from it.
fn plan_item_store(
    old: Option<&NodItemState>,
    nod_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    let body = decode_item(nod_id, stored_body.as_bytes())?;
    let primary_record = primary_record(stored_body, metadata);
    let scope = crate::partitioning::item_scope(body.owner)?;
    if let Some(old) = old {
        let old_scope = crate::partitioning::item_scope(old.owner)?;
        if old_scope != scope {
            batch.push(AtomicWriteOperation::delete(
                namespace(NODS_NAMESPACE)?.with_scope(old_scope),
                item_key(nod_id)?,
            ));
        }
    }
    batch.push(AtomicWriteOperation::put_record(
        namespace(NODS_NAMESPACE)?.with_scope(scope),
        item_key(nod_id)?,
        primary_record,
    ));
    batch.push(AtomicWriteOperation::put(
        namespace(crate::partitioning::NOD_LOCATIONS_NAMESPACE)?,
        item_key(nod_id)?,
        Value::new(
            crate::partitioning::owner_shard(body.owner)?
                .to_be_bytes()
                .to_vec(),
        )?,
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
    Ok(batch)
}

/// Plans one Nod item delete, with the index derivable from the old body.
fn plan_item_delete(
    old: Option<&NodItemState>,
    nod_id: WwdEntityId,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    if let Some(old) = old {
        batch.push(AtomicWriteOperation::delete(
            namespace(NODS_BY_OWNER_NAMESPACE)?,
            owner_index_key(old.owner, nod_id)?,
        ));
    }
    let primary = match old {
        Some(old) => {
            namespace(NODS_NAMESPACE)?.with_scope(crate::partitioning::item_scope(old.owner)?)
        }
        None => namespace(NODS_NAMESPACE)?,
    };
    batch.push(AtomicWriteOperation::delete(primary, item_key(nod_id)?));
    batch.push(AtomicWriteOperation::delete(
        namespace(crate::partitioning::NOD_LOCATIONS_NAMESPACE)?,
        item_key(nod_id)?,
    ));
    Ok(batch)
}

/// Plans one Nod bucket primary store without storage access. Decodes the
/// stored body to validate its canonical bucket identity.
fn plan_bucket_store(
    _old: Option<&NodBucketState>,
    bucket_id: WwdEntityId,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, NodRepositoryError> {
    let mut batch = AtomicWriteBatch::new();
    decode_bucket(bucket_id, stored_body.as_bytes())?;
    batch.push(AtomicWriteOperation::put_record(
        namespace(NOD_BUCKETS_NAMESPACE)?,
        bucket_storage_key(bucket_id)?,
        primary_record(stored_body, metadata),
    ));
    Ok(batch)
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
