use alloy_primitives::{Address, B256, U256};
use outbe_gem::{
    canonical_bucket, canonical_item, from_canonical_bucket, from_canonical_item, GemBucketState,
    GemCodecError, GemItemState,
};
use outbe_primitives::entity_codec::{
    decode_stored_gem_bucket_v1, decode_stored_gem_v1, encode_gem_bucket_v1, encode_gem_v1,
    StoredBody, BODY_SCHEMA_V1,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

fn gem_id(seed: u64) -> WwdEntityId {
    WwdEntityId::from(U256::from(seed))
}

fn item(gem_id: WwdEntityId, owner: Address) -> GemItemState {
    GemItemState {
        gem_id,
        owner,
        gem_type: 1,
        promis_load_minor: U256::MAX,
        issuance_currency: 0,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
        bucket_key: B256::repeat_byte(0x33),
        state: 0,
        called_at: 0,
        settled_at: 0,
    }
}

fn bucket(key: B256, worldwide_day: WorldwideDay) -> GemBucketState {
    GemBucketState {
        settled_gems: 0,
        bucket_key: key,
        worldwide_day,
        entry_price_minor: U256::ZERO,
        reference_currency: 978,
    }
}

fn stored_item(body: &GemItemState) -> Vec<u8> {
    StoredBody::new(
        BODY_SCHEMA_V1,
        encode_gem_v1(&canonical_item(body)).unwrap(),
    )
    .unwrap()
    .encode()
}

fn stored_bucket(body: &GemBucketState) -> Vec<u8> {
    StoredBody::new(
        BODY_SCHEMA_V1,
        encode_gem_bucket_v1(&canonical_bucket(body)).unwrap(),
    )
    .unwrap()
    .encode()
}

#[test]
fn the_bucket_reference_currency_only_appends_to_the_canonical_payload() {
    let mut body = bucket(B256::repeat_byte(0x21), WorldwideDay::new(20_260_715));
    body.reference_currency = 0;
    let unpriced = encode_gem_bucket_v1(&canonical_bucket(&body)).unwrap();
    body.reference_currency = 840;
    let priced = encode_gem_bucket_v1(&canonical_bucket(&body)).unwrap();
    assert_eq!(priced, [unpriced.as_slice(), &[0x20, 0xC8, 0x06]].concat());
}

#[test]
fn canonical_stored_bodies_roundtrip_field_boundaries() {
    let zero = GemItemState {
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
    };
    let max = GemItemState {
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
    };
    for body in [zero, max] {
        let decoded =
            from_canonical_item(decode_stored_gem_v1(&stored_item(&body)).unwrap()).unwrap();
        assert_eq!(decoded, body);
    }
    for body in [
        bucket(B256::ZERO, WorldwideDay::new(0)),
        GemBucketState {
            settled_gems: u64::MAX,
            entry_price_minor: U256::MAX,
            reference_currency: u16::MAX,
            ..bucket(B256::repeat_byte(u8::MAX), WorldwideDay::new(u32::MAX))
        },
    ] {
        let decoded =
            from_canonical_bucket(decode_stored_gem_bucket_v1(&stored_bucket(&body)).unwrap())
                .unwrap();
        assert_eq!(decoded, body);
    }

    let mut wide = canonical_item(&item(gem_id(1), Address::ZERO));
    wide.gem_type = u32::from(u8::MAX) + 1;
    let stored = StoredBody::new(BODY_SCHEMA_V1, encode_gem_v1(&wide).unwrap())
        .unwrap()
        .encode();
    assert!(matches!(
        from_canonical_item(decode_stored_gem_v1(&stored).unwrap()),
        Err(GemCodecError::CanonicalFieldOutOfRange { field: "gem_type" })
    ));
    wide.gem_type = 2;
    wide.state = outbe_gem::GemState::Called as u32;
    assert!(encode_gem_v1(&wide).is_err());
}
