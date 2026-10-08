use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, TributeBodyV1};
use outbe_ocomp_protocol::result::ContributorActionV1;
use outbe_primitives::time::WorldwideDay;

pub fn tribute_population(day: WorldwideDay, tribute_count: u32) -> Vec<TributeBodyV1> {
    let mut tributes = (0..tribute_count)
        .map(|index| {
            let mut owner_bytes = [0_u8; 20];
            owner_bytes[16..].copy_from_slice(&(index + 1).to_be_bytes());
            let owner = Address::from(owner_bytes);
            TributeBodyV1 {
                tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                owner,
                worldwide_day: day,
                issuance_amount_minor: U256::from(1),
                issuance_currency: if index % 2 == 0 { 840 } else { 826 },
                nominal_amount_minor: U256::from((index % 7) + 1),
                reference_currency: if index % 3 == 0 { 978 } else { 392 },
                tribute_price_minor: U256::from(1),
                exclude_from_intex_issuance: false,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|tribute| tribute.tribute_id);
    tributes
}
pub fn contributor_population(tributes: &[TributeBodyV1]) -> Vec<ContributorActionV1> {
    let mut contributors_by_owner = tributes
        .iter()
        .map(|tribute| ContributorActionV1 {
            owner: tribute.owner,
            source_tribute_id: *tribute.tribute_id,
            nominal_amount_minor: tribute.nominal_amount_minor,
        })
        .collect::<Vec<_>>();
    contributors_by_owner
        .sort_by_key(|contributor| (contributor.owner, contributor.source_tribute_id));
    contributors_by_owner
}
