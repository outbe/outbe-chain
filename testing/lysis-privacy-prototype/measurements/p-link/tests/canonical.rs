//! Independent oracle: exact pinned production entity derive/codec/hash.
use alloy_primitives::B256;
use ark_bn254_v6::Fr as Fr6;
use ark_ff_v6::PrimeField;
use outbe_p_link_measurements::{
    circuit::{binding_hash, draft_hash, fixture},
    integer::fr_integer,
};
use outbe_protocol::{protocol::entity::Entity, OutbeV1, Suite};
use outbe_protocol_derive::Entity;

#[derive(Entity)]
struct TributeDraftClaim {
    #[outbe(id_seed)]
    id: B256,
    #[outbe(body, owner, pos = 0)]
    derived_owner: B256,
    #[outbe(body, pos = 1)]
    worldwide_day: u64,
    #[outbe(body, pos = 2)]
    currency: u16,
    #[outbe(body, pos = 3)]
    base: u64,
    #[outbe(body, pos = 4)]
    atto: u64,
    #[outbe(body, pos = 5)]
    su_ids: Vec<B256>,
}
fn b256(f: ark_bn254::Fr) -> B256 {
    let b = fr_integer(f).to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - b.len()..].copy_from_slice(&b);
    B256::from(out)
}
#[test]
fn draft_and_binding_match_current_enclave_schema() {
    let mut fixture = fixture();
    for count in 0..=4 {
        fixture.public.source_count = count;
        for i in 0..4 {
            fixture.public.source_ids[i] = ark_bn254::Fr::from(if i < (count as usize) {
                31 + i as u64 * 6
            } else {
                0
            });
        }
        let p = &fixture.public;
        let w = &fixture.private;
        let draft = TributeDraftClaim {
            id: b256(w.draft_id),
            derived_owner: b256(p.derived_owner),
            worldwide_day: p.day,
            currency: p.currency,
            base: w.base,
            atto: w.atto,
            su_ids: p.source_ids[..count as usize]
                .iter()
                .copied()
                .map(b256)
                .collect(),
        };
        let expected = <TributeDraftClaim as Entity<OutbeV1>>::entity_hash(&draft).unwrap();
        assert_eq!(
            Fr6::from_le_bytes_mod_order(&fr_integer(draft_hash(p, w)).to_bytes_le()),
            expected
        );
        let sender = p.sender.to_bytes_be();
        let mut address = [0u8; 20];
        address[20 - sender.len()..].copy_from_slice(&sender);
        let expected_binding = OutbeV1::binding(&address, draft.id.as_ref(), p.chain_id).unwrap();
        let actual =
            Fr6::from_le_bytes_mod_order(&fr_integer(binding_hash(p, w.draft_id)).to_bytes_le());
        assert_eq!(actual, expected_binding);
    }
}
