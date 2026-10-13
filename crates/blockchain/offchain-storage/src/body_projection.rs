//! Repository-owned projection overlays. Validation precedes every overlay mutation.
use crate::{AtomicWriteBatch, StorageMetadata, Value};
use std::collections::BTreeMap;

pub type Records<I, T> = BTreeMap<I, Option<(T, Option<StorageMetadata>)>>;
pub type StorePlanner<I, T, E> =
    fn(Option<&T>, I, Value, Option<StorageMetadata>) -> Result<AtomicWriteBatch, E>;
pub struct BodyKind<I, T, E> {
    pub entity: &'static str,
    pub decode: fn(I, &[u8]) -> Result<T, E>,
    pub validate: fn(I, &T) -> Result<(), E>,
    pub plan_store: StorePlanner<I, T, E>,
    pub plan_delete: fn(Option<&T>, I) -> Result<AtomicWriteBatch, E>,
    pub untracked: fn(&'static str, I) -> E,
}

pub fn tracked<'a, I: Ord + Copy, T, E>(
    records: &'a Records<I, T>,
    kind: &BodyKind<I, T, E>,
    identity: I,
) -> Result<Option<(&'a T, Option<&'a StorageMetadata>)>, E> {
    Ok(records
        .get(&identity)
        .ok_or_else(|| (kind.untracked)(kind.entity, identity))?
        .as_ref()
        .map(|(body, metadata)| (body, metadata.as_ref())))
}
fn validated_current<'a, I: Ord + Copy, T, E>(
    records: &'a Records<I, T>,
    kind: &BodyKind<I, T, E>,
    identity: I,
) -> Result<Option<&'a T>, E> {
    let old = tracked(records, kind, identity)?.map(|(body, _)| body);
    if let Some(old) = old {
        (kind.validate)(identity, old)?;
    }
    Ok(old)
}
pub fn store<I: Ord + Copy, T, E>(
    records: &mut Records<I, T>,
    kind: &BodyKind<I, T, E>,
    identity: I,
    stored_body: Value,
    metadata: Option<StorageMetadata>,
) -> Result<AtomicWriteBatch, E> {
    let body = (kind.decode)(identity, stored_body.as_bytes())?;
    let old = validated_current(records, kind, identity)?;
    let batch = (kind.plan_store)(old, identity, stored_body, metadata.clone())?;
    records.insert(identity, Some((body, metadata)));
    Ok(batch)
}
pub fn delete<I: Ord + Copy, T, E>(
    records: &mut Records<I, T>,
    kind: &BodyKind<I, T, E>,
    identity: I,
) -> Result<AtomicWriteBatch, E> {
    let old = validated_current(records, kind, identity)?;
    let batch = (kind.plan_delete)(old, identity)?;
    records.insert(identity, None);
    Ok(batch)
}

/// Defines a domain-owned session with private fields and a crate-private constructor.
/// Keeping construction in the owning crate prevents callers from forging prior bodies.
#[macro_export]
macro_rules! body_projection_session {
    ($session:ident, $from_records:ident, $id:ty, $item:ty, $bucket:ty, $error:ty, $items:ident, $buckets:ident) => {
        pub struct $session {
            items: $crate::body_projection::Records<$id, $item>,
            buckets: $crate::body_projection::Records<$id, $bucket>,
        }
        pub(crate) fn $from_records(
            item_ids: &[$id],
            items: Vec<Option<($item, Option<$crate::StorageMetadata>)>>,
            bucket_ids: &[$id],
            buckets: Vec<Option<($bucket, Option<$crate::StorageMetadata>)>>,
        ) -> $session {
            $session {
                items: item_ids.iter().copied().zip(items).collect(),
                buckets: bucket_ids.iter().copied().zip(buckets).collect(),
            }
        }
        impl $session {
            pub fn current_item(&self, id: $id) -> Result<Option<&$item>, $error> {
                Ok(self.current_item_with_metadata(id)?.map(|(body, _)| body))
            }
            pub fn current_item_with_metadata(
                &self,
                id: $id,
            ) -> Result<Option<(&$item, Option<&$crate::StorageMetadata>)>, $error> {
                $crate::body_projection::tracked(&self.items, &$items, id)
            }
            pub fn current_bucket(&self, id: $id) -> Result<Option<&$bucket>, $error> {
                Ok(self.current_bucket_with_metadata(id)?.map(|(body, _)| body))
            }
            pub fn current_bucket_with_metadata(
                &self,
                id: $id,
            ) -> Result<Option<(&$bucket, Option<&$crate::StorageMetadata>)>, $error> {
                $crate::body_projection::tracked(&self.buckets, &$buckets, id)
            }
            pub fn store_item(
                &mut self,
                id: $id,
                body: $crate::Value,
                metadata: Option<$crate::StorageMetadata>,
            ) -> Result<$crate::AtomicWriteBatch, $error> {
                $crate::body_projection::store(&mut self.items, &$items, id, body, metadata)
            }
            pub fn delete_item(&mut self, id: $id) -> Result<$crate::AtomicWriteBatch, $error> {
                $crate::body_projection::delete(&mut self.items, &$items, id)
            }
            pub fn store_bucket(
                &mut self,
                id: $id,
                body: $crate::Value,
                metadata: Option<$crate::StorageMetadata>,
            ) -> Result<$crate::AtomicWriteBatch, $error> {
                $crate::body_projection::store(&mut self.buckets, &$buckets, id, body, metadata)
            }
            pub fn delete_bucket(&mut self, id: $id) -> Result<$crate::AtomicWriteBatch, $error> {
                $crate::body_projection::delete(&mut self.buckets, &$buckets, id)
            }
        }
    };
}

/// Storage layout and canonical decoding for an owner-indexed primary body.
pub struct OwnedBody<I, T, E> {
    pub decode: fn(I, &[u8]) -> Result<T, E>,
    pub primary: fn(I) -> Result<(crate::Namespace, crate::Key), E>,
    pub owner_key: fn(&T, I) -> Result<crate::Key, E>,
    pub owner_namespace: fn() -> Result<crate::Namespace, E>,
}
impl<I: Copy, T, E: From<crate::StorageError>> OwnedBody<I, T, E> {
    pub fn store(
        &self,
        old: Option<&T>,
        id: I,
        record: crate::StoredValue,
    ) -> Result<AtomicWriteBatch, E> {
        let body = (self.decode)(id, record.value.as_bytes())?;
        let (namespace, key) = (self.primary)(id)?;
        let owner_namespace = (self.owner_namespace)()?;
        let owner_key = (self.owner_key)(&body, id)?;
        let mut batch = AtomicWriteBatch::new();
        batch.push(crate::AtomicWriteOperation::put_record(
            namespace, key, record,
        ));
        batch.push(crate::AtomicWriteOperation::put(
            owner_namespace.clone(),
            owner_key.clone(),
            Value::new(Vec::new())?,
        ));
        if let Some(old) = old {
            let old_key = (self.owner_key)(old, id)?;
            if old_key != owner_key {
                batch.push(crate::AtomicWriteOperation::delete(
                    owner_namespace,
                    old_key,
                ));
            }
        }
        Ok(batch)
    }
    pub fn delete(&self, old: Option<&T>, id: I) -> Result<AtomicWriteBatch, E> {
        let mut batch = AtomicWriteBatch::new();
        if let Some(old) = old {
            batch.push(crate::AtomicWriteOperation::delete(
                (self.owner_namespace)()?,
                (self.owner_key)(old, id)?,
            ));
        }
        let (namespace, key) = (self.primary)(id)?;
        batch.push(crate::AtomicWriteOperation::delete(namespace, key));
        Ok(batch)
    }
}
