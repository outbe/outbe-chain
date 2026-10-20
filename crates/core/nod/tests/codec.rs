use alloy_primitives::{Address, B256, U256};
use outbe_nod::{
    canonical_group, canonical_item, from_canonical_group, from_canonical_item, NodGroupState,
    NodItemState,
};
use outbe_primitives::entity_codec::{
    decode_stored_nod_group_v1, decode_stored_nod_item_v2, encode_nod_group_v1, encode_nod_item_v2,
    StoredBody,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

fn entity(seed: U256, worldwide_day: WorldwideDay) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(worldwide_day, seed.to_be_bytes::<32>())
}

fn group_id(key: B256, worldwide_day: WorldwideDay) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(worldwide_day, key.0)
}

fn group_key_for(id: WwdEntityId) -> B256 {
    let mut key = [0_u8; 32];
    key[4..].copy_from_slice(&id.body());
    B256::from(key)
}

fn group(group_id: WwdEntityId) -> NodGroupState {
    NodGroupState {
        settled_nods: 0,
        group_key: group_key_for(group_id),
        full_day_after_issue: group_id.worldwide_day(),
        reference_currency: 978,
    }
}

fn stored_nod(body: &NodItemState) -> Vec<u8> {
    StoredBody::new(
        outbe_primitives::entity_codec::NOD_BODY_SCHEMA_V2,
        encode_nod_item_v2(&canonical_item(body)).unwrap(),
    )
    .unwrap()
    .encode()
}

fn stored_group(body: &NodGroupState) -> Vec<u8> {
    StoredBody::new(
        outbe_primitives::entity_codec::BODY_SCHEMA_V1,
        encode_nod_group_v1(&canonical_group(body)).unwrap(),
    )
    .unwrap()
    .encode()
}

#[test]
fn the_group_reference_currency_only_appends_to_the_canonical_payload() {
    let mut body = group(group_id(
        B256::repeat_byte(0x21),
        WorldwideDay::new(20_260_715),
    ));
    body.reference_currency = 0;
    let unpriced = encode_nod_group_v1(&canonical_group(&body)).unwrap();
    body.reference_currency = 840;
    let priced = encode_nod_group_v1(&canonical_group(&body)).unwrap();

    // Tag = (4 << 3) | 0 = 0x20; 840 as a varint = 0xC8 0x06.
    assert_eq!(priced, [unpriced.as_slice(), &[0x20, 0xC8, 0x06]].concat());
}

#[test]
fn canonical_stored_bodies_roundtrip_all_nod_field_boundaries() {
    for body in [
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id: entity(U256::ZERO, WorldwideDay::new(0)),
                owner: Address::ZERO,
                gratis_load_minor: U256::ZERO,
                worldwide_day: WorldwideDay::new(0),
                league_id: 0,
                group_key: B256::ZERO,
                issuance_currency: 0,
                reference_currency: 0,
                issued_at: 0,
            },
            U256::ZERO,
        ),
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id: entity(U256::MAX, WorldwideDay::new(u32::MAX)),
                owner: Address::repeat_byte(u8::MAX),
                gratis_load_minor: U256::MAX,
                worldwide_day: WorldwideDay::new(u32::MAX),
                league_id: u16::MAX,
                group_key: B256::repeat_byte(u8::MAX),
                issuance_currency: u16::MAX,
                reference_currency: u16::MAX,
                issued_at: u64::MAX,
            },
            U256::ZERO,
        ),
    ] {
        let stored = stored_nod(&body);
        let decoded = from_canonical_item(decode_stored_nod_item_v2(&stored).unwrap());
        assert_eq!(decoded.nod_id, body.nod_id);
        assert_eq!(decoded.owner, body.owner);
        assert_eq!(
            outbe_nod::api::calculation_amount(&decoded).unwrap(),
            outbe_nod::api::calculation_amount(&body).unwrap()
        );
        assert_eq!(decoded.worldwide_day, body.worldwide_day);
        assert_eq!(decoded.league_id, body.league_id);
        assert_eq!(decoded.group_key, body.group_key);
        assert_eq!(decoded.issuance_currency, body.issuance_currency);
        assert_eq!(decoded.reference_currency, body.reference_currency);
        assert_eq!(decoded.issued_at, body.issued_at);
    }

    for body in [
        NodGroupState {
            settled_nods: 0,
            group_key: B256::ZERO,
            full_day_after_issue: WorldwideDay::new(0),
            reference_currency: 0,
        },
        NodGroupState {
            reference_currency: u16::MAX,
            ..group(group_id(
                B256::repeat_byte(u8::MAX),
                WorldwideDay::new(u32::MAX),
            ))
        },
    ] {
        let stored = stored_group(&body);
        let decoded = from_canonical_group(decode_stored_nod_group_v1(&stored).unwrap());
        assert_eq!(decoded.group_key, body.group_key);
        assert_eq!(decoded.full_day_after_issue, body.full_day_after_issue);
        assert_eq!(decoded.reference_currency, body.reference_currency);
    }
}

#[test]
fn nod_body_is_self_contained_and_carries_only_encrypted_amounts() {
    let amount = U256::from_be_bytes([0x39; 32]);
    let params = outbe_nod::NodIssueParams {
        owner: Address::repeat_byte(0x81),
        worldwide_day: WorldwideDay::new(20_261_007),
        league_id: 4,
        gratis_load_minor: amount,
        entry_price_minor: U256::from(17),
        issuance_currency: 840,
        reference_currency: 978,
    };
    let encrypted = outbe_nod::test_support::encrypted_fixture(&params, 7);
    let body = outbe_primitives::entity_codec::NodItemBodyV2 {
        encrypted: encrypted.clone(),
        group_key: B256::repeat_byte(0x74),
        issued_at: 99,
        is_settled: false,
    };
    let payload = encode_nod_item_v2(&body).unwrap();
    let creator = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from([0x6b; 32]));
    assert!(!payload
        .windows(32)
        .any(|bytes| bytes == amount.to_be_bytes::<32>()));
    assert!(!payload.windows(32).any(|bytes| bytes == creator.as_bytes()));
    let stored = outbe_primitives::entity_codec::StoredBody::new(
        outbe_primitives::entity_codec::NOD_BODY_SCHEMA_V2,
        payload,
    )
    .unwrap();
    let decoded = from_canonical_item(
        outbe_primitives::entity_codec::decode_stored_nod_item_v2(&stored.encode()).unwrap(),
    );
    assert_eq!(decoded.encrypted, encrypted);
    assert_eq!(
        outbe_nod::api::calculation_amount(&decoded).unwrap(),
        amount
    );
    let mut corrupted = body.clone();
    corrupted.encrypted.encrypted_gratis_amount.truncate(55);
    assert!(encode_nod_item_v2(&corrupted).is_err());
    let legacy = outbe_primitives::entity_codec::StoredBody::new(
        outbe_primitives::entity_codec::BODY_SCHEMA_V1,
        encode_nod_item_v2(&body).unwrap(),
    )
    .unwrap();
    assert!(outbe_primitives::entity_codec::decode_stored_nod_item_v2(&legacy.encode()).is_err());
}
