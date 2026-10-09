use alloy_primitives::{Address, LogData, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, decode_nod_bucket_v1, derive_poseidon_entity_id, StoredBody, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
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

pub(super) struct StoredEntityEvent {
    pub(super) source: ProjectionSource,
    pub(super) identity: WwdEntityId,
    pub(super) stored_body: Value,
    pub(super) previous_commitment: B256,
}

pub(super) struct DeletedEntityEvent {
    pub(super) identity: WwdEntityId,
    pub(super) previous_commitment: B256,
}

pub(super) enum ProjectionEvent {
    TributeStored(StoredEntityEvent),
    TributeDeleted(DeletedEntityEvent),
    TributePartitionRetired { worldwide_day: WorldwideDay },
    NodStored(StoredEntityEvent),
    NodDeleted(DeletedEntityEvent),
    BucketStored(StoredEntityEvent),
    BucketDeleted(DeletedEntityEvent),
}

impl ProjectionEvent {
    pub(super) fn identity(&self) -> Option<EntityIdentity> {
        match self {
            Self::TributeStored(event) => Some(EntityIdentity::Tribute(event.identity)),
            Self::TributeDeleted(event) => Some(EntityIdentity::Tribute(event.identity)),
            Self::TributePartitionRetired { .. } => None,
            Self::NodStored(event) => Some(EntityIdentity::Nod(event.identity)),
            Self::NodDeleted(event) => Some(EntityIdentity::Nod(event.identity)),
            Self::BucketStored(event) => Some(EntityIdentity::Bucket(event.identity)),
            Self::BucketDeleted(event) => Some(EntityIdentity::Bucket(event.identity)),
        }
    }
}

pub(super) fn is_projection_pair(emitter: Address, signature: B256) -> bool {
    let signatures: &[B256] = if emitter == TRIBUTE_ADDRESS {
        &[
            ITribute::TributeBodyStored::SIGNATURE_HASH,
            ITribute::TributeBodyDeleted::SIGNATURE_HASH,
            ITribute::TributePartitionRetired::SIGNATURE_HASH,
        ]
    } else if emitter == NOD_ADDRESS {
        &[
            INod::NodBodyStored::SIGNATURE_HASH,
            INod::NodBodyDeleted::SIGNATURE_HASH,
            INod::NodBucketBodyStored::SIGNATURE_HASH,
            INod::NodBucketBodyDeleted::SIGNATURE_HASH,
        ]
    } else {
        &[]
    };
    signatures.contains(&signature)
}

pub(super) fn decode_event(
    source: ProjectionSource,
    data: &LogData,
) -> Result<Option<ProjectionEvent>, ProjectionError> {
    if source.emitter == TRIBUTE_ADDRESS {
        return decode_tribute_event(source, data);
    }
    if source.emitter == NOD_ADDRESS {
        return decode_nod_event(source, data);
    }
    Ok(None)
}

fn decode_tribute_event(
    source: ProjectionSource,
    data: &LogData,
) -> Result<Option<ProjectionEvent>, ProjectionError> {
    let decoded = if source.event_signature == ITribute::TributeBodyStored::SIGNATURE_HASH {
        let event = ITribute::TributeBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
        let tribute_id = WwdEntityId::from(event.tributeId);
        let canonical =
            outbe_tribute::record::decode_payload(event.schemaVersion, &event.canonicalPayload)
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
            (event.previousCommitment, event.newCommitment),
            event.schemaVersion,
        )?;
        Some(ProjectionEvent::TributeStored(StoredEntityEvent {
            source,
            identity: tribute_id,
            stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
            previous_commitment: event.previousCommitment,
        }))
    } else if source.event_signature == ITribute::TributeBodyDeleted::SIGNATURE_HASH {
        let event = ITribute::TributeBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        Some(ProjectionEvent::TributeDeleted(DeletedEntityEvent {
            identity: WwdEntityId::from(event.tributeId),
            previous_commitment: event.previousCommitment,
        }))
    } else if source.event_signature == ITribute::TributePartitionRetired::SIGNATURE_HASH {
        let event = ITribute::TributePartitionRetired::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        Some(ProjectionEvent::TributePartitionRetired {
            worldwide_day: event.worldwideDay.into(),
        })
    } else {
        None
    };
    Ok(decoded)
}

fn decode_nod_event(
    source: ProjectionSource,
    data: &LogData,
) -> Result<Option<ProjectionEvent>, ProjectionError> {
    let decoded = if source.event_signature == INod::NodBodyStored::SIGNATURE_HASH {
        Some(decode_nod_item(source, data)?)
    } else if source.event_signature == INod::NodBodyDeleted::SIGNATURE_HASH {
        let event = INod::NodBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        Some(ProjectionEvent::NodDeleted(DeletedEntityEvent {
            identity: WwdEntityId::from(event.nodId),
            previous_commitment: event.previousCommitment,
        }))
    } else if source.event_signature == INod::NodBucketBodyStored::SIGNATURE_HASH {
        Some(decode_nod_bucket(source, data)?)
    } else if source.event_signature == INod::NodBucketBodyDeleted::SIGNATURE_HASH {
        let event = INod::NodBucketBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        Some(ProjectionEvent::BucketDeleted(DeletedEntityEvent {
            identity: WwdEntityId::from(event.bucketId),
            previous_commitment: event.previousCommitment,
        }))
    } else {
        None
    };
    Ok(decoded)
}

fn decode_nod_item(
    source: ProjectionSource,
    data: &LogData,
) -> Result<ProjectionEvent, ProjectionError> {
    let event = INod::NodBodyStored::decode_log_data(data)
        .map_err(|error| malformed_event(source, error))?;
    validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
    let nod_id = WwdEntityId::from(event.nodId);
    let canonical = outbe_compressed_entities::decode_nod_item_v2(&event.canonicalPayload)
        .map_err(|error| malformed_event(source, error))?;
    if canonical.encrypted.terms.nod_id != nod_id {
        return Err(malformed_event(
            source,
            "Nod event identity/payload mismatch",
        ));
    }
    validate_poseidon_identity(
        source,
        "Nod item",
        nod_id,
        canonical.encrypted.terms.owner,
        canonical.encrypted.terms.worldwide_day,
    )?;
    validate_stored_commitment(
        source,
        nod_id,
        &event.canonicalPayload,
        (event.previousCommitment, event.newCommitment),
        event.schemaVersion,
    )?;
    Ok(ProjectionEvent::NodStored(StoredEntityEvent {
        source,
        identity: nod_id,
        stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
        previous_commitment: event.previousCommitment,
    }))
}

fn decode_nod_bucket(
    source: ProjectionSource,
    data: &LogData,
) -> Result<ProjectionEvent, ProjectionError> {
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
        (event.previousCommitment, event.newCommitment),
        event.schemaVersion,
    )?;
    Ok(ProjectionEvent::BucketStored(StoredEntityEvent {
        source,
        identity: bucket_id,
        stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
        previous_commitment: event.previousCommitment,
    }))
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
    let encrypted_schema = encrypted_body_schema(source);
    if schema_version != BODY_SCHEMA_V1 && encrypted_schema != Some(schema_version) {
        return Err(malformed_event(
            source,
            format!("unsupported body schema {schema_version}"),
        ));
    }
    Ok(())
}

fn encrypted_body_schema(source: ProjectionSource) -> Option<u32> {
    match (source.emitter, source.event_signature) {
        (TRIBUTE_ADDRESS, ITribute::TributeBodyStored::SIGNATURE_HASH) => {
            Some(outbe_compressed_entities::TRIBUTE_BODY_SCHEMA_V2)
        }
        (NOD_ADDRESS, INod::NodBodyStored::SIGNATURE_HASH) => {
            Some(outbe_compressed_entities::NOD_BODY_SCHEMA_V2)
        }
        _ => None,
    }
}

pub(super) fn validate_stored_commitment(
    source: ProjectionSource,
    identity: WwdEntityId,
    payload: &[u8],
    commitments: (B256, B256),
    schema_version: u32,
) -> Result<(), ProjectionError> {
    let (previous, new) = commitments;
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
