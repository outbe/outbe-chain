//! Pure off-chain mutation planning for Tribute projection.

use alloy_primitives::U256;
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, StorageMetadata, StoredValue, Value,
};

use crate::{
    repository::{
        day_index_key, encode_body, namespace, owner_index_key, primary_key,
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

/// One complete Tribute projection mutation.
pub enum TributeMutation<'a> {
    /// Store a complete body and its optional primary metadata.
    Store {
        /// Indexed event identity.
        token_id: U256,
        /// Complete post-mutation body.
        body: &'a TributeData,
        /// Metadata attached only to the primary body.
        metadata: Option<StorageMetadata>,
    },
    /// Delete one body identity and all indexes derivable from the old body.
    Delete {
        /// Indexed event identity.
        token_id: U256,
    },
}

/// Plans all primary and index mutations without reading or writing storage.
pub fn plan_tribute_mutation(
    old: Option<&TributeData>,
    mutation: TributeMutation<'_>,
) -> Result<AtomicWriteBatch, TributeRepositoryError> {
    let token_id = match &mutation {
        TributeMutation::Store { token_id, .. } | TributeMutation::Delete { token_id } => *token_id,
    };
    if let Some(old) = old {
        validate_identity(token_id, old)?;
    }

    let mut batch = AtomicWriteBatch::new();
    match mutation {
        TributeMutation::Store {
            token_id,
            body,
            metadata,
        } => {
            validate_identity(token_id, body)?;
            let primary_record = match metadata {
                Some(metadata) => StoredValue::with_metadata(encode_body(body)?, metadata),
                None => StoredValue::plain(encode_body(body)?),
            };
            batch.push(AtomicWriteOperation::put_record(
                namespace(TRIBUTES_NAMESPACE)?,
                primary_key(token_id)?,
                primary_record,
            ));
            let empty = Value::new(Vec::new())?;
            batch.push(AtomicWriteOperation::put(
                namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                owner_index_key(body.owner, token_id)?,
                empty.clone(),
            ));
            batch.push(AtomicWriteOperation::put(
                namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                day_index_key(body.worldwide_day, token_id)?,
                empty,
            ));
            if let Some(old) = old {
                if old.owner != body.owner {
                    batch.push(AtomicWriteOperation::delete(
                        namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                        owner_index_key(old.owner, token_id)?,
                    ));
                }
                if old.worldwide_day != body.worldwide_day {
                    batch.push(AtomicWriteOperation::delete(
                        namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                        day_index_key(old.worldwide_day, token_id)?,
                    ));
                }
            }
        }
        TributeMutation::Delete { token_id } => {
            if let Some(old) = old {
                batch.push(AtomicWriteOperation::delete(
                    namespace(TRIBUTES_BY_OWNER_NAMESPACE)?,
                    owner_index_key(old.owner, token_id)?,
                ));
                batch.push(AtomicWriteOperation::delete(
                    namespace(TRIBUTES_BY_DAY_NAMESPACE)?,
                    day_index_key(old.worldwide_day, token_id)?,
                ));
            }
            batch.push(AtomicWriteOperation::delete(
                namespace(TRIBUTES_NAMESPACE)?,
                primary_key(token_id)?,
            ));
        }
    }
    Ok(batch)
}

fn validate_identity(expected: U256, body: &TributeData) -> Result<(), TributeRepositoryError> {
    if body.token_id != expected {
        return Err(TributeRepositoryError::PrimaryKeyBodyMismatch {
            expected,
            actual: body.token_id,
        });
    }
    Ok(())
}
