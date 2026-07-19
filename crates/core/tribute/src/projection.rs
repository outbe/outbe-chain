//! Pure off-chain mutation planning for Tribute projection.

use std::collections::BTreeMap;

use outbe_compressed_entities::EntityId36;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::{
    repository::TributeRecordWithMetadata,
    repository::{
        day_index_key, decode_body, namespace, owner_index_key, primary_key,
        TRIBUTES_BY_DAY_NAMESPACE, TRIBUTES_BY_OWNER_NAMESPACE, TRIBUTES_NAMESPACE,
    },
    TributeData, TributeRepositoryError,
};

/// Code-defined namespaces owned by the Tribute repository.
pub const TRIBUTE_PROJECTION_NAMESPACES: [&str; 3] = [
    TRIBUTES_NAMESPACE,
    TRIBUTES_BY_OWNER_NAMESPACE,
    TRIBUTES_BY_DAY_NAMESPACE,
];

/// Repository-owned prior state plus an in-block overlay for Tribute projection.
///
/// Callers can mutate only identities loaded through
/// [`crate::TributeRepositoryReader::projection_session`]. They cannot supply or omit an
/// arbitrary semantic prior body.
pub struct TributeProjectionSession {
    records: BTreeMap<EntityId36, Option<TributeRecordWithMetadata>>,
}

impl TributeProjectionSession {
    pub(crate) fn from_records(
        tribute_ids: &[EntityId36],
        records: Vec<Option<TributeRecordWithMetadata>>,
    ) -> Self {
        Self {
            records: tribute_ids.iter().copied().zip(records).collect(),
        }
    }

    /// Returns the current semantic body from the repository snapshot or in-block overlay.
    pub fn current(
        &self,
        tribute_id: EntityId36,
    ) -> Result<Option<&TributeData>, TributeRepositoryError> {
        Ok(self
            .current_with_metadata(tribute_id)?
            .map(|(body, _)| body))
    }

    /// Returns the current body and provenance without exposing a constructible prior snapshot.
    pub fn current_with_metadata(
        &self,
        tribute_id: EntityId36,
    ) -> Result<Option<(&TributeData, Option<&StorageMetadata>)>, TributeRepositoryError> {
        match self
            .records
            .get(&tribute_id)
            .ok_or(TributeRepositoryError::UntrackedProjectionIdentity { tribute_id })?
        {
            Some((body, metadata)) => Ok(Some((body, metadata.as_ref()))),
            None => Ok(None),
        }
    }

    /// Plans one canonical store and advances the overlay only after full validation.
    pub fn store(
        &mut self,
        tribute_id: EntityId36,
        stored_body: Value,
        metadata: Option<StorageMetadata>,
    ) -> Result<AtomicWriteBatch, TributeRepositoryError> {
        let body = decode_body(tribute_id, stored_body.as_bytes())?;
        let old = self.current(tribute_id)?;
        let batch = plan_tribute_mutation(
            old,
            TributeMutation::Store {
                tribute_id,
                stored_body,
                metadata: metadata.clone(),
            },
        )?;
        self.records.insert(tribute_id, Some((body, metadata)));
        Ok(batch)
    }

    /// Plans one delete from the owned prior snapshot and advances the overlay to absence.
    pub fn delete(
        &mut self,
        tribute_id: EntityId36,
    ) -> Result<AtomicWriteBatch, TributeRepositoryError> {
        let old = self.current(tribute_id)?;
        let batch = plan_tribute_mutation(old, TributeMutation::Delete { tribute_id })?;
        self.records.insert(tribute_id, None);
        Ok(batch)
    }
}

/// One complete Tribute projection mutation.
enum TributeMutation {
    /// Store a complete body and its optional primary metadata.
    Store {
        /// Indexed event identity.
        tribute_id: EntityId36,
        /// Exact canonical StoredBody bytes. The planner decodes this value and
        /// derives every semantic index from it.
        stored_body: Value,
        /// Metadata attached only to the primary body.
        metadata: Option<StorageMetadata>,
    },
    /// Delete one body identity and all indexes derivable from the old body.
    Delete {
        /// Indexed event identity.
        tribute_id: EntityId36,
    },
}

/// Plans all primary and index mutations without reading or writing storage.
fn plan_tribute_mutation(
    old: Option<&TributeData>,
    mutation: TributeMutation,
) -> Result<AtomicWriteBatch, TributeRepositoryError> {
    let tribute_id = match &mutation {
        TributeMutation::Store { tribute_id, .. } | TributeMutation::Delete { tribute_id } => {
            *tribute_id
        }
    };
    if let Some(old) = old {
        validate_identity(tribute_id, old)?;
    }

    let mut batch = AtomicWriteBatch::new();
    match mutation {
        TributeMutation::Store {
            tribute_id,
            stored_body,
            metadata,
        } => {
            let body = decode_body(tribute_id, stored_body.as_bytes())?;
            let primary_record = match metadata {
                Some(metadata) => StoredValue::with_metadata(stored_body, metadata),
                None => StoredValue::plain(stored_body),
            };
            batch.push(AtomicWriteOperation::put_record(
                namespace(TRIBUTES_NAMESPACE)?,
                primary_key(tribute_id)?,
                primary_record,
            ));
            let empty = Value::new(Vec::new())?;
            batch.push(AtomicWriteOperation::put(
                namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                owner_index_key(body.owner, tribute_id)?,
                empty.clone(),
            ));
            batch.push(AtomicWriteOperation::put(
                namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                day_index_key(body.worldwide_day, tribute_id)?,
                empty,
            ));
            if let Some(old) = old {
                if old.owner != body.owner {
                    batch.push(AtomicWriteOperation::delete(
                        namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                        owner_index_key(old.owner, tribute_id)?,
                    ));
                }
                if old.worldwide_day != body.worldwide_day {
                    batch.push(AtomicWriteOperation::delete(
                        namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                        day_index_key(old.worldwide_day, tribute_id)?,
                    ));
                }
            }
        }
        TributeMutation::Delete { tribute_id } => {
            if let Some(old) = old {
                batch.push(AtomicWriteOperation::delete(
                    namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                    owner_index_key(old.owner, tribute_id)?,
                ));
                batch.push(AtomicWriteOperation::delete(
                    namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                    day_index_key(old.worldwide_day, tribute_id)?,
                ));
            }
            batch.push(AtomicWriteOperation::delete(
                namespace(TRIBUTES_NAMESPACE)?,
                primary_key(tribute_id)?,
            ));
        }
    }
    Ok(batch)
}

fn validate_identity(
    expected: EntityId36,
    body: &TributeData,
) -> Result<(), TributeRepositoryError> {
    if body.tribute_id != expected {
        return Err(TributeRepositoryError::PrimaryKeyBodyMismatch {
            expected,
            actual: body.tribute_id,
        });
    }
    Ok(())
}
