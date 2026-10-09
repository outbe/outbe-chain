use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, encode_tribute_v1, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{canonical_body, TributeData};

pub(crate) fn tribute_commitment(body: &TributeData) -> B256 {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            body.tribute_id,
            &payload,
        )
        .unwrap()
        .as_bytes(),
    )
}

/// Tribute body with the fixed amounts and currencies of the projection tests.
pub(crate) fn tribute_body(tribute_id: WwdEntityId, owner: Address, day: u32) -> TributeData {
    TributeData {
        tribute_id,
        owner,
        worldwide_day: WorldwideDay::new(day),
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    }
}
