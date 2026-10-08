use super::*;

pub(super) fn prepare_input(input: BodyInput<'_>) -> Result<PreparedBody> {
    match input {
        BodyInput::Tribute(body) => prepare_tribute(body.clone()),
        BodyInput::EncryptedTribute(body) => prepare_encrypted_tribute(body),
        BodyInput::NodItem(body) => prepare_nod_item(body.clone()),
        BodyInput::EncryptedNodItem(body) => prepare_encrypted_nod_item(body),
        BodyInput::NodBucket(body) => prepare_nod_bucket(body.clone()),
    }
}

fn prepare_tribute(body: TributeBodyV1) -> Result<PreparedBody> {
    let payload = encode_tribute_v1(&body).map_err(input_error)?;
    let stored_body =
        StoredBody::new(crate::BODY_SCHEMA_V1, payload.clone()).map_err(input_error)?;
    let entity_id = body.tribute_id;
    let commitment = calculate_commitment(entity_id, &payload)?;
    let memberships = vec![
        IndexRecord::owner(IndexKind::TributeByOwner, body.owner, entity_id),
        IndexRecord::day(body.worldwide_day, entity_id),
    ];
    Ok(PreparedBody {
        collection: Collection::Tribute,
        entity_id,
        stored_body,
        commitment,
        memberships,
    })
}

fn prepare_encrypted_tribute(
    body: &outbe_primitives::tribute_encryption::EncryptedTributeV2,
) -> Result<PreparedBody> {
    let payload = crate::encode_tribute_v2(body).map_err(input_error)?;
    let stored_body =
        StoredBody::new(crate::TRIBUTE_BODY_SCHEMA_V2, payload.clone()).map_err(input_error)?;
    let entity_id = body.context.tribute_id;
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored_body.schema_version(),
        entity_id,
        &payload,
    )
    .map_err(|error| fatal(error.to_string()))?;
    Ok(PreparedBody {
        collection: Collection::Tribute,
        entity_id,
        stored_body,
        commitment,
        memberships: vec![
            IndexRecord::owner(IndexKind::TributeByOwner, body.context.owner, entity_id),
            IndexRecord::day(body.context.worldwide_day, entity_id),
        ],
    })
}

fn prepare_nod_item(body: NodItemBodyV1) -> Result<PreparedBody> {
    let payload = encode_nod_item_v1(&body).map_err(input_error)?;
    let stored_body =
        StoredBody::new(crate::BODY_SCHEMA_V1, payload.clone()).map_err(input_error)?;
    let entity_id = body.nod_id;
    let commitment = calculate_commitment(entity_id, &payload)?;
    let memberships = vec![
        IndexRecord::owner(IndexKind::NodByOwner, body.owner, entity_id),
        IndexRecord::nod_all(entity_id),
    ];
    Ok(PreparedBody {
        collection: Collection::NodItem,
        entity_id,
        stored_body,
        commitment,
        memberships,
    })
}

fn prepare_nod_bucket(body: NodBucketBodyV1) -> Result<PreparedBody> {
    let payload = encode_nod_bucket_v1(&body).map_err(input_error)?;
    let stored_body =
        StoredBody::new(crate::BODY_SCHEMA_V1, payload.clone()).map_err(input_error)?;
    let entity_id = body.entity_id();
    let commitment = calculate_commitment(entity_id, &payload)?;
    Ok(PreparedBody {
        collection: Collection::NodBucket,
        entity_id,
        stored_body,
        commitment,
        memberships: Vec::new(),
    })
}

pub(super) fn verify_stored(
    entity: EntityRef,
    stored_body: StoredBody,
    expected: Commitment,
    origin: BodyOrigin,
) -> Result<VerifiedBody> {
    let encrypted_tribute = matches!(entity, EntityRef::Tribute(_))
        && stored_body.schema_version() == crate::TRIBUTE_BODY_SCHEMA_V2;
    let encrypted_nod = matches!(entity, EntityRef::NodItem(_))
        && stored_body.schema_version() == crate::NOD_BODY_SCHEMA_V2;
    if stored_body.schema_version() != BODY_SCHEMA_V1 && !encrypted_tribute && !encrypted_nod {
        return Err(origin.invalid(format!(
            "unsupported stored body schema {}",
            stored_body.schema_version()
        )));
    }
    let payload = stored_body.payload();
    let entity_id = entity.entity_id();
    let (decoded_id, verified_payload) = decode_stored_payload(entity, &stored_body, origin)?;
    if decoded_id != entity_id {
        return Err(origin.invalid(format!(
            "body identity {decoded_id} does not match requested {entity_id}"
        )));
    }
    let actual = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored_body.schema_version(),
        entity_id,
        payload,
    )
    .map_err(|error| origin.invalid(error.to_string()))?;
    if actual != expected {
        // Preserve the exact authenticated input, not a re-encoded replacement.
        // Bodies are canonical on-chain event payloads, never enclave key material.
        return Err(origin.invalid(format!(
            "body commitment mismatch for {entity_id}; [CE_BODY_DIAGNOSTIC] entity={entity:?} origin={origin:?} scheme={ACTIVE_COMMITMENT_SCHEME} schema={} expected=0x{} actual=0x{} payload_len={} payload_hex={} stored_body_hex={} decoded={verified_payload:?}",
            stored_body.schema_version(),
            hex::encode(expected.as_bytes()),
            hex::encode(actual.as_bytes()),
            payload.len(),
            hex::encode(payload),
            hex::encode(stored_body.encode()),
        )));
    }
    Ok(VerifiedBody {
        entity,
        commitment: expected,
        stored_body,
        payload: verified_payload,
    })
}

fn decode_stored_payload(
    entity: EntityRef,
    stored_body: &StoredBody,
    origin: BodyOrigin,
) -> Result<(WwdEntityId, crate::api::VerifiedPayload)> {
    let payload = stored_body.payload();
    let decoded = match entity {
        EntityRef::Tribute(_) if stored_body.schema_version() == crate::TRIBUTE_BODY_SCHEMA_V2 => {
            let body = crate::decode_tribute_v2(payload)
                .map_err(|error| origin.invalid(error.to_string()))?;
            (
                body.context.tribute_id,
                crate::api::encrypted_tribute_payload(body),
            )
        }
        EntityRef::Tribute(_) => {
            let body =
                decode_tribute_v1(payload).map_err(|error| origin.invalid(error.to_string()))?;
            (body.tribute_id, tribute_payload(body))
        }
        EntityRef::NodItem(_) if stored_body.schema_version() == crate::NOD_BODY_SCHEMA_V2 => {
            let body = crate::decode_nod_item_v2(payload)
                .map_err(|error| origin.invalid(error.to_string()))?;
            (
                body.encrypted.terms.nod_id,
                crate::api::encrypted_nod_item_payload(body),
            )
        }
        EntityRef::NodItem(_) => {
            let body =
                decode_nod_item_v1(payload).map_err(|error| origin.invalid(error.to_string()))?;
            (body.nod_id, nod_item_payload(body))
        }
        EntityRef::NodBucket(_) => {
            let body =
                decode_nod_bucket_v1(payload).map_err(|error| origin.invalid(error.to_string()))?;
            (body.entity_id(), nod_bucket_payload(body))
        }
    };
    Ok(decoded)
}

fn prepare_encrypted_nod_item(body: &crate::NodItemBodyV2) -> Result<PreparedBody> {
    let payload = crate::encode_nod_item_v2(body).map_err(input_error)?;
    let stored_body =
        StoredBody::new(crate::NOD_BODY_SCHEMA_V2, payload.clone()).map_err(input_error)?;
    let entity_id = body.encrypted.terms.nod_id;
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored_body.schema_version(),
        entity_id,
        &payload,
    )
    .map_err(|error| fatal(error.to_string()))?;
    Ok(PreparedBody {
        collection: Collection::NodItem,
        entity_id,
        stored_body,
        commitment,
        memberships: vec![
            IndexRecord::owner(IndexKind::NodByOwner, body.encrypted.terms.owner, entity_id),
            IndexRecord::nod_all(entity_id),
        ],
    })
}
