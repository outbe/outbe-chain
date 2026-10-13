//! Gem identities are opaque IDs; only buckets carry a worldwide day.
use super::*;
use outbe_compressed_entities::{decode_gem_bucket_v1, decode_gem_v1};

pub(super) fn decode_gem_event(
    source: ProjectionSource,
    data: &LogData,
) -> Result<Option<ProjectionEvent>, ProjectionError> {
    let event = if source.event_signature == IGem::GemBodyStored::SIGNATURE_HASH {
        let event = IGem::GemBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
        let identity = WwdEntityId::from(event.gemId);
        let body = decode_gem_v1(&event.canonicalPayload)
            .map_err(|error| malformed_event(source, error))?;
        if body.gem_id != identity {
            return Err(malformed_event(
                source,
                "Gem event identity/payload mismatch",
            ));
        }
        validate_stored_commitment(
            source,
            identity,
            &event.canonicalPayload,
            (event.previousCommitment, event.newCommitment),
            event.schemaVersion,
        )?;
        ProjectionEvent::GemStored(StoredEntityEvent {
            source,
            identity,
            stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
            previous_commitment: event.previousCommitment,
        })
    } else if source.event_signature == IGem::GemBucketBodyStored::SIGNATURE_HASH {
        let event = IGem::GemBucketBodyStored::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_versions(source, event.commitmentSchemeVersion, event.schemaVersion)?;
        let identity = WwdEntityId::from(event.bucketId);
        let body = decode_gem_bucket_v1(&event.canonicalPayload)
            .map_err(|error| malformed_event(source, error))?;
        if body.entity_id() != identity {
            return Err(malformed_event(
                source,
                "Gem bucket event identity/payload mismatch",
            ));
        }
        validate_stored_commitment(
            source,
            identity,
            &event.canonicalPayload,
            (event.previousCommitment, event.newCommitment),
            event.schemaVersion,
        )?;
        ProjectionEvent::GemBucketStored(StoredEntityEvent {
            source,
            identity,
            stored_body: stored_event_body(source, event.schemaVersion, &event.canonicalPayload)?,
            previous_commitment: event.previousCommitment,
        })
    } else if source.event_signature == IGem::GemBodyDeleted::SIGNATURE_HASH {
        let event = IGem::GemBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        ProjectionEvent::GemDeleted(DeletedEntityEvent {
            identity: WwdEntityId::from(event.gemId),
            previous_commitment: event.previousCommitment,
        })
    } else if source.event_signature == IGem::GemBucketBodyDeleted::SIGNATURE_HASH {
        let event = IGem::GemBucketBodyDeleted::decode_log_data(data)
            .map_err(|error| malformed_event(source, error))?;
        validate_deleted_commitment(source, event.previousCommitment)?;
        ProjectionEvent::GemBucketDeleted(DeletedEntityEvent {
            identity: WwdEntityId::from(event.bucketId),
            previous_commitment: event.previousCommitment,
        })
    } else {
        return Ok(None);
    };
    Ok(Some(event))
}
