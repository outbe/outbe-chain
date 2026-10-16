use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    entity_codec::{decode_tribute_v1, encode_tribute_v1},
    time::WorldwideDay,
    wwd_entity_id::WwdEntityId,
};
use outbe_tribute::TributeData;

#[test]
fn canonical_payload_roundtrips_all_tribute_field_boundaries() {
    for body in [
        TributeData {
            tribute_id: WwdEntityId::from_day_and_digest(WorldwideDay::new(0), B256::ZERO),
            owner: Address::ZERO,
            worldwide_day: WorldwideDay::new(0),
            issuance_amount_minor: U256::ZERO,
            issuance_currency: 0,
            nominal_amount_minor: U256::ZERO,
            reference_currency: 0,
            tribute_price_minor: U256::ZERO,
        },
        TributeData {
            tribute_id: WwdEntityId::from_day_and_digest(
                WorldwideDay::new(u32::MAX),
                B256::repeat_byte(255),
            ),
            owner: Address::repeat_byte(u8::MAX),
            worldwide_day: WorldwideDay::new(u32::MAX),
            issuance_amount_minor: U256::MAX,
            issuance_currency: u16::MAX,
            nominal_amount_minor: U256::MAX,
            reference_currency: u16::MAX,
            tribute_price_minor: U256::MAX,
        },
    ] {
        let payload = encode_tribute_v1(&outbe_tribute::canonical_body(&body)).unwrap();
        let decoded = outbe_tribute::from_canonical_body(decode_tribute_v1(&payload).unwrap());
        assert_eq!(decoded.tribute_id, body.tribute_id);
        assert_eq!(decoded.owner, body.owner);
        assert_eq!(decoded.worldwide_day, body.worldwide_day);
        assert_eq!(decoded.issuance_amount_minor, body.issuance_amount_minor);
        assert_eq!(decoded.issuance_currency, body.issuance_currency);
        assert_eq!(decoded.nominal_amount_minor, body.nominal_amount_minor);
        assert_eq!(decoded.reference_currency, body.reference_currency);
        assert_eq!(decoded.tribute_price_minor, body.tribute_price_minor);
    }
}
