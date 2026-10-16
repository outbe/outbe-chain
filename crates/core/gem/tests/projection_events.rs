use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, decode_gem_bucket_v1, decode_gem_v1, encode_gem_bucket_v1, encode_gem_v1,
    WwdEntityId, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_gem::{
    canonical_bucket, canonical_bucket_id, canonical_item, from_canonical_bucket,
    from_canonical_item, precompile::IGem, GemBucketState, GemItemState,
};
use outbe_primitives::time::WorldwideDay;

fn stored_item_event(record: &GemItemState) -> (Vec<u8>, B256) {
    let payload = encode_gem_v1(&canonical_item(record)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        record.gem_id,
        &payload,
    )
    .unwrap();
    (payload, B256::from(*commitment.as_bytes()))
}

fn assert_item_roundtrip(record: GemItemState) {
    let (payload, commitment) = stored_item_event(&record);
    let event = IGem::GemBodyStored {
        gemId: record.gem_id.to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: B256::ZERO,
        newCommitment: commitment,
        canonicalPayload: Bytes::from(payload.clone()),
    };
    let log = event.encode_log_data();
    let decoded = IGem::GemBodyStored::decode_log_data(&log).unwrap();
    let reconstructed =
        from_canonical_item(decode_gem_v1(&decoded.canonicalPayload).unwrap()).unwrap();
    assert_eq!(log.topics(), &[IGem::GemBodyStored::SIGNATURE_HASH]);
    assert_eq!(WwdEntityId::from(decoded.gemId), record.gem_id);
    assert_eq!(decoded.commitmentSchemeVersion, ACTIVE_COMMITMENT_SCHEME);
    assert_eq!(decoded.schemaVersion, BODY_SCHEMA_V1);
    assert_eq!(decoded.previousCommitment, B256::ZERO);
    assert_eq!(decoded.newCommitment, commitment);
    assert_eq!(decoded.canonicalPayload.as_ref(), payload);
    assert_eq!(reconstructed, record);
}

fn assert_bucket_roundtrip(record: GemBucketState) {
    let bucket_id = canonical_bucket_id(&record);
    let payload = encode_gem_bucket_v1(&canonical_bucket(&record)).unwrap();
    let commitment = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        bucket_id,
        &payload,
    )
    .unwrap();
    let event = IGem::GemBucketBodyStored {
        bucketId: bucket_id.to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: B256::ZERO,
        newCommitment: B256::from(*commitment.as_bytes()),
        canonicalPayload: Bytes::from(payload.clone()),
    };
    let log = event.encode_log_data();
    let decoded = IGem::GemBucketBodyStored::decode_log_data(&log).unwrap();
    let reconstructed =
        from_canonical_bucket(decode_gem_bucket_v1(&decoded.canonicalPayload).unwrap()).unwrap();
    assert_eq!(log.topics(), &[IGem::GemBucketBodyStored::SIGNATURE_HASH]);
    assert_eq!(WwdEntityId::from(decoded.bucketId), bucket_id);
    assert_eq!(decoded.commitmentSchemeVersion, ACTIVE_COMMITMENT_SCHEME);
    assert_eq!(decoded.schemaVersion, BODY_SCHEMA_V1);
    assert_eq!(decoded.previousCommitment, B256::ZERO);
    assert_eq!(decoded.newCommitment, B256::from(*commitment.as_bytes()));
    assert_eq!(decoded.canonicalPayload.as_ref(), payload);
    assert_eq!(reconstructed, record);
}

#[test]
fn stored_events_carry_exact_canonical_gem_bodies_and_commitments() {
    assert_item_roundtrip(GemItemState {
        gem_id: WwdEntityId::from(U256::ZERO),
        owner: Address::ZERO,
        gem_type: 0,
        promis_load_minor: U256::ZERO,
        issuance_currency: 0,
        reference_currency: 0,
        issued_at: 0,
        bucket_key: B256::ZERO,
        state: 0,
        called_at: 0,
        settled_at: 0,
    });
    assert_item_roundtrip(GemItemState {
        gem_id: WwdEntityId::from(U256::MAX),
        owner: Address::repeat_byte(u8::MAX),
        gem_type: u8::MAX,
        promis_load_minor: U256::MAX,
        issuance_currency: u16::MAX,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
        bucket_key: B256::repeat_byte(u8::MAX),
        state: outbe_gem::GemState::Settled as u8,
        called_at: u64::MAX,
        settled_at: u64::MAX,
    });
    assert_bucket_roundtrip(GemBucketState {
        settled_gems: 0,
        bucket_key: B256::ZERO,
        worldwide_day: WorldwideDay::new(0),
        entry_price_minor: U256::ZERO,
        reference_currency: 0,
    });
    assert_bucket_roundtrip(GemBucketState {
        settled_gems: u64::MAX,
        bucket_key: B256::repeat_byte(u8::MAX),
        worldwide_day: WorldwideDay::new(u32::MAX),
        entry_price_minor: U256::MAX,
        reference_currency: u16::MAX,
    });
}

#[test]
fn transition_event_signatures_topics_and_delete_payloads_are_pinned() {
    assert_eq!(
        IGem::GemBodyStored::SIGNATURE_HASH,
        keccak256("GemBodyStored(uint256,uint32,uint32,bytes32,bytes32,bytes)")
    );
    assert_eq!(
        IGem::GemBucketBodyStored::SIGNATURE_HASH,
        keccak256("GemBucketBodyStored(uint256,uint32,uint32,bytes32,bytes32,bytes)")
    );
    assert_eq!(
        IGem::GemBodyDeleted::SIGNATURE_HASH,
        keccak256("GemBodyDeleted(uint256,bytes32)")
    );
    assert_eq!(
        IGem::GemBucketBodyDeleted::SIGNATURE_HASH,
        keccak256("GemBucketBodyDeleted(uint256,bytes32)")
    );

    let gem_id = WwdEntityId::from(U256::MAX);
    let previous_item = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        gem_id,
        b"non-empty canonical gem item fixture",
    )
    .unwrap();
    let item_log = IGem::GemBodyDeleted {
        gemId: gem_id.to_u256(),
        previousCommitment: B256::from(*previous_item.as_bytes()),
    }
    .encode_log_data();
    let decoded_item = IGem::GemBodyDeleted::decode_log_data(&item_log).unwrap();
    assert_eq!(item_log.topics(), &[IGem::GemBodyDeleted::SIGNATURE_HASH]);
    assert_eq!(WwdEntityId::from(decoded_item.gemId), gem_id);
    assert_eq!(
        decoded_item.previousCommitment,
        B256::from(*previous_item.as_bytes())
    );

    let day = WorldwideDay::new(20_260_715);
    let bucket_key = B256::repeat_byte(0xab);
    let bucket_id = WwdEntityId::from_day_and_digest(day, bucket_key);
    let previous_bucket = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        bucket_id,
        b"non-empty canonical gem bucket fixture",
    )
    .unwrap();
    let bucket_log = IGem::GemBucketBodyDeleted {
        bucketId: bucket_id.to_u256(),
        previousCommitment: B256::from(*previous_bucket.as_bytes()),
    }
    .encode_log_data();
    let decoded_bucket = IGem::GemBucketBodyDeleted::decode_log_data(&bucket_log).unwrap();
    assert_eq!(
        bucket_log.topics(),
        &[IGem::GemBucketBodyDeleted::SIGNATURE_HASH]
    );
    assert_eq!(WwdEntityId::from(decoded_bucket.bucketId), bucket_id);
    assert_eq!(
        decoded_bucket.previousCommitment,
        B256::from(*previous_bucket.as_bytes())
    );
}
