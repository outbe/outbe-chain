use alloy_primitives::{Address, LogData, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, decode_nod_bucket_v1, decode_nod_item_v1, derive_poseidon_entity_id,
    StoredBody, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_nod::precompile::INod;
use outbe_offchain_storage::Value;
use outbe_primitives::addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::precompile::ITribute;

use super::state::ProjectionSource;
use super::{FinalizedBlock, ProjectionError};

#[derive(Clone, Copy)]
pub(super) enum EntityIdentity {
    Tribute(WwdEntityId),
    Nod(WwdEntityId),
    Bucket(WwdEntityId),
}

pub(super) enum ProjectionEvent {
    TributeStored {
        source: ProjectionSource,
        tribute_id: WwdEntityId,
        stored_body: Value,
        previous_commitment: B256,
    },
    TributeDeleted {
        tribute_id: WwdEntityId,
        previous_commitment: B256,
    },
    TributePartitionRetired {
        worldwide_day: WorldwideDay,
    },
    NodStored {
        source: ProjectionSource,
        nod_id: WwdEntityId,
        stored_body: Value,
        previous_commitment: B256,
    },
    NodDeleted {
        nod_id: WwdEntityId,
        previous_commitment: B256,
    },
    BucketStored {
        source: ProjectionSource,
        bucket_id: WwdEntityId,
        stored_body: Value,
        previous_commitment: B256,
    },
    BucketDeleted {
        bucket_id: WwdEntityId,
        previous_commitment: B256,
    },
}

impl ProjectionEvent {
    pub(super) fn identity(&self) -> Option<EntityIdentity> {
        match self {
            Self::TributeStored { tribute_id, .. } => Some(EntityIdentity::Tribute(*tribute_id)),
            Self::TributeDeleted { tribute_id, .. } => Some(EntityIdentity::Tribute(*tribute_id)),
            Self::TributePartitionRetired { .. } => None,
            Self::NodStored { nod_id, .. } => Some(EntityIdentity::Nod(*nod_id)),
            Self::NodDeleted { nod_id, .. } => Some(EntityIdentity::Nod(*nod_id)),
            Self::BucketStored { bucket_id, .. } | Self::BucketDeleted { bucket_id, .. } => {
                Some(EntityIdentity::Bucket(*bucket_id))
            }
        }
    }
}

pub(super) fn is_projection_pair(emitter: Address, signature: B256) -> bool {
    (emitter == TRIBUTE_ADDRESS
        && (signature == ITribute::TributeBodyStored::SIGNATURE_HASH
            || signature == ITribute::TributeBodyDeleted::SIGNATURE_HASH
            || signature == ITribute::TributePartitionRetired::SIGNATURE_HASH))
        || (emitter == NOD_ADDRESS
            && (signature == INod::NodBodyStored::SIGNATURE_HASH
                || signature == INod::NodBodyDeleted::SIGNATURE_HASH
                || signature == INod::NodBucketBodyStored::SIGNATURE_HASH
                || signature == INod::NodBucketBodyDeleted::SIGNATURE_HASH))
}

pub(super) fn decode_event(
    source: ProjectionSource,
    data: &LogData,
) -> Result<Option<ProjectionEvent>, ProjectionError> {
    let decoded = if source.emitter == TRIBUTE_ADDRESS
        && source.event_signature == ITribute::TributeBodyStored::SIGNATURE_HASH
    {
        let event = ITribute::TributeBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
        let tribute_id = WwdEntityId::from(event.tributeId);
        let canonical = outbe_tribute::TributeRecord::decode_payload(
            event.schemaVersion,
            &event.canonicalPayload,
        )
        .map_err(|error| malformed_event(source, error))?;
        if canonical.tribute_id != tribute_id {
            return Err(malformed_event(
                source,
                "Tribute event identity/payload mismatch",
            ));
        }
        validate_poseidon_identity(
            source,
            "Tribute",
            tribute_id,
            canonical.owner,
            canonical.worldwide_day,
        )?;
        validate_stored_commitment(
            source,
            tribute_id,
            &event.canonicalPayload,
            event.previousCommitment,
            event.newCommitment,
            event.schemaVersion,
        )?;
        Some(ProjectionEvent::TributeStored {
            source,
            tribute_id,
            stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
            previous_commitment: event.previousCommitment,
        })
    } else if source.emitter == TRIBUTE_ADDRESS
        && source.event_signature == ITribute::TributeBodyDeleted::SIGNATURE_HASH
    {
        let event = ITribute::TributeBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        Some(ProjectionEvent::TributeDeleted {
            tribute_id: WwdEntityId::from(event.tributeId),
            previous_commitment: event.previousCommitment,
        })
    } else if source.emitter == TRIBUTE_ADDRESS
        && source.event_signature == ITribute::TributePartitionRetired::SIGNATURE_HASH
    {
        let event = ITribute::TributePartitionRetired::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::TributePartitionRetired {
            worldwide_day: event.worldwideDay.into(),
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBodyStored::SIGNATURE_HASH
    {
        let event = INod::NodBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
        let nod_id = WwdEntityId::from(event.nodId);
        let canonical = decode_nod_item_v1(&event.canonicalPayload)
            .map_err(|error| malformed_event(source, error))?;
        if canonical.nod_id != nod_id {
            return Err(malformed_event(
                source,
                "Nod event identity/payload mismatch",
            ));
        }
        validate_poseidon_identity(
            source,
            "Nod item",
            nod_id,
            canonical.owner,
            canonical.worldwide_day,
        )?;
        validate_stored_commitment(
            source,
            nod_id,
            &event.canonicalPayload,
            event.previousCommitment,
            event.newCommitment,
            event.schemaVersion,
        )?;
        Some(ProjectionEvent::NodStored {
            source,
            nod_id,
            stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
            previous_commitment: event.previousCommitment,
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBodyDeleted::SIGNATURE_HASH
    {
        let event = INod::NodBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        Some(ProjectionEvent::NodDeleted {
            nod_id: WwdEntityId::from(event.nodId),
            previous_commitment: event.previousCommitment,
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBucketBodyStored::SIGNATURE_HASH
    {
        let event = INod::NodBucketBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
        let bucket_id = WwdEntityId::from(event.bucketId);
        let canonical = decode_nod_bucket_v1(&event.canonicalPayload)
            .map_err(|error| malformed_event(source, error))?;
        if canonical.entity_id() != bucket_id {
            return Err(malformed_event(
                source,
                "Nod bucket event identity/payload mismatch",
            ));
        }
        validate_stored_commitment(
            source,
            bucket_id,
            &event.canonicalPayload,
            event.previousCommitment,
            event.newCommitment,
            event.schemaVersion,
        )?;
        Some(ProjectionEvent::BucketStored {
            source,
            bucket_id,
            stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
            previous_commitment: event.previousCommitment,
        })
    } else if source.emitter == NOD_ADDRESS
        && source.event_signature == INod::NodBucketBodyDeleted::SIGNATURE_HASH
    {
        let event = INod::NodBucketBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        Some(ProjectionEvent::BucketDeleted {
            bucket_id: WwdEntityId::from(event.bucketId),
            previous_commitment: event.previousCommitment,
        })
    } else {
        None
    };
    Ok(decoded)
}

pub(super) fn validate_poseidon_identity(
    source: ProjectionSource,
    entity: &'static str,
    actual: WwdEntityId,
    owner: Address,
    worldwide_day: outbe_primitives::time::WorldwideDay,
) -> Result<(), ProjectionError> {
    let expected = derive_poseidon_entity_id(owner, worldwide_day)
        .map_err(|error| malformed_event(source, error))?;
    if actual != expected {
        return Err(malformed_event(
            source,
            format!("{entity} canonical identity mismatch: expected {expected}, found {actual}"),
        ));
    }
    Ok(())
}

pub(super) fn stored_event_body(
    source: ProjectionSource,
    schema_version: u32,
    payload: &[u8],
) -> Result<Value, ProjectionError> {
    let stored = StoredBody::new(schema_version, payload.to_vec())
        .map_err(|error| malformed_event(source, error))?;
    Value::new(stored.encode()).map_err(ProjectionError::Storage)
}

pub(super) fn validate_versions(
    source: ProjectionSource,
    commitment_scheme_version: u32,
    schema_version: u32,
) -> Result<(), ProjectionError> {
    if commitment_scheme_version != ACTIVE_COMMITMENT_SCHEME {
        return Err(malformed_event(
            source,
            format!("unsupported commitment scheme {commitment_scheme_version}"),
        ));
    }
    let encrypted_tribute = source.emitter == TRIBUTE_ADDRESS
        && source.event_signature == ITribute::TributeBodyStored::SIGNATURE_HASH
        && schema_version == outbe_compressed_entities::TRIBUTE_BODY_SCHEMA_V2;
    if schema_version != BODY_SCHEMA_V1 && !encrypted_tribute {
        return Err(malformed_event(
            source,
            format!("unsupported body schema {schema_version}"),
        ));
    }
    Ok(())
}

pub(super) fn validate_stored_commitment(
    source: ProjectionSource,
    identity: WwdEntityId,
    payload: &[u8],
    previous: B256,
    new: B256,
    schema_version: u32,
) -> Result<(), ProjectionError> {
    if !previous.is_zero() {
        outbe_compressed_entities::Commitment::try_from(previous.0)
            .map_err(|error| malformed_event(source, error))?;
    }
    let expected = body_commitment(ACTIVE_COMMITMENT_SCHEME, schema_version, identity, payload)
        .map_err(|error| malformed_event(source, error))?;
    if new != B256::from(*expected.as_bytes()) {
        return Err(malformed_event(
            source,
            "new commitment does not match payload",
        ));
    }
    Ok(())
}

pub(super) fn validate_deleted_commitment(
    source: ProjectionSource,
    previous: B256,
) -> Result<(), ProjectionError> {
    outbe_compressed_entities::Commitment::try_from(previous.0)
        .map(|_| ())
        .map_err(|error| malformed_event(source, error))
}

pub(super) fn malformed_event(
    source: ProjectionSource,
    error: impl std::fmt::Display,
) -> ProjectionError {
    ProjectionError::MalformedProjectionEvent {
        event_source: Box::new(source),
        reason: error.to_string(),
    }
}

pub(super) fn validate_normalized_block(block: &FinalizedBlock) -> Result<(), ProjectionError> {
    let mut expected_log_index = 0_u64;
    for (expected_index, receipt) in block.receipts.iter().enumerate() {
        let expected_index =
            u64::try_from(expected_index).map_err(|_| ProjectionError::TransactionIndexOverflow)?;
        if receipt.transaction_index != expected_index {
            return Err(ProjectionError::InvalidTransactionOrder {
                expected: expected_index,
                actual: receipt.transaction_index,
            });
        }
        for log in &receipt.logs {
            if log.log_index != expected_log_index {
                return Err(ProjectionError::InvalidLogOrder {
                    expected: expected_log_index,
                    actual: log.log_index,
                });
            }
            expected_log_index = expected_log_index
                .checked_add(1)
                .ok_or(ProjectionError::LogIndexOverflow)?;
        }
    }
    Ok(())
}
