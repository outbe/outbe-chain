use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, encode_gem_bucket_v1, encode_gem_v1, GemBodyV1, GemBucketBodyV1, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_gem::precompile::IGem;
use outbe_primitives::time::WorldwideDay;

pub fn item(seed: u8) -> GemBodyV1 {
    GemBodyV1 {
        gem_id: WwdEntityId::from(B256::repeat_byte(seed)),
        owner: Address::repeat_byte(0x41),
        gem_type: 1,
        promis_load_minor: U256::from(100),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 1_752_534_000,
        bucket_key: B256::repeat_byte(4),
        state: 0,
        called_at: 0,
        settled_at: 0,
    }
}
pub fn bucket() -> GemBucketBodyV1 {
    GemBucketBodyV1 {
        bucket_key: B256::repeat_byte(4),
        worldwide_day: WorldwideDay::new(20260715),
        entry_price_minor: U256::from(3),
        reference_currency: 978,
        settled_gems: 0,
    }
}
pub fn commitment(id: WwdEntityId, payload: &[u8]) -> B256 {
    B256::from(
        *body_commitment(ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1, id, payload)
            .unwrap()
            .as_bytes(),
    )
}
pub fn stored(body: &GemBodyV1, previous: B256) -> IGem::GemBodyStored {
    let payload = encode_gem_v1(body).unwrap();
    IGem::GemBodyStored {
        gemId: body.gem_id.to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: previous,
        newCommitment: commitment(body.gem_id, &payload),
        canonicalPayload: Bytes::from(payload),
    }
}
pub fn bucket_stored(body: &GemBucketBodyV1, previous: B256) -> IGem::GemBucketBodyStored {
    let payload = encode_gem_bucket_v1(body).unwrap();
    IGem::GemBucketBodyStored {
        bucketId: body.entity_id().to_u256(),
        commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
        schemaVersion: BODY_SCHEMA_V1,
        previousCommitment: previous,
        newCommitment: commitment(body.entity_id(), &payload),
        canonicalPayload: Bytes::from(payload),
    }
}
pub fn deleted(body: &GemBodyV1) -> LogData {
    IGem::GemBodyDeleted {
        gemId: body.gem_id.to_u256(),
        previousCommitment: stored(body, B256::ZERO).newCommitment,
    }
    .encode_log_data()
}
pub fn bucket_deleted(body: &GemBucketBodyV1) -> LogData {
    IGem::GemBucketBodyDeleted {
        bucketId: body.entity_id().to_u256(),
        previousCommitment: bucket_stored(body, B256::ZERO).newCommitment,
    }
    .encode_log_data()
}
