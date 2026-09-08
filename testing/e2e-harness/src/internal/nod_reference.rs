//! Independent V1 Nod commitment encoding for E2E expectations.
//!
//! Only the wire value type and Keccak primitive are shared with production.
//! Do not use the production codec, registry, list or materialization helpers
//! here: their operation order and domain framing are part of what we check.
//! A root calculated from observed actions verifies the commitment, not the
//! correctness of their economic fields. Those require fixture-derived values.

use alloy_primitives::{keccak256, B256, U256};
use outbe_compressed_entities::TributeBodyV1;
use outbe_ocomp_protocol::result::NodActionV1;

const NOD_LIST_KIND: [u8; 2] = 1u16.to_be_bytes();
const RECORD_BYTES: usize = 266;

/// Exact single-league fixture model. The caller verifies the source population
/// and every owner's snapshotted league before supplying these input terms.
pub(crate) fn single_league_actions(
    tributes: &[TributeBodyV1],
    league: u16,
    budget: U256,
    entry_price: U256,
    issued_at: u64,
) -> Vec<NodActionV1> {
    assert!(
        matches!(tributes.len(), 1 | 10),
        "declared Nod field fixture"
    );
    assert!(league > 0, "available input league");
    assert!(!entry_price.is_zero(), "available input USD price");
    let scale = U256::from(1_000_000);
    let total = tributes.iter().fold(U256::ZERO, |sum, tribute| {
        assert_eq!(tribute.reference_currency, 840, "USD input model");
        assert!(!tribute.nominal_amount_minor.is_zero());
        sum.checked_add(tribute.nominal_amount_minor)
            .expect("fixture nominal sum")
    });
    // With one occupied league its nominal share is one, so the distribution
    // fraction is the target itself. Preserve the two floors independently.
    let fraction = budget.checked_mul(scale).expect("fixture scaled budget") / total;
    let mut sorted = tributes.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|tribute| tribute.tribute_id);
    sorted
        .windows(2)
        .for_each(|pair| assert_ne!(pair[0].tribute_id, pair[1].tribute_id));
    let mut remaining = budget;
    sorted
        .into_iter()
        .enumerate()
        .map(|(ordinal, tribute)| {
            let load = tribute
                .nominal_amount_minor
                .checked_mul(fraction)
                .expect("fixture scaled load")
                / scale;
            assert!(!load.is_zero(), "positive expected load");
            remaining = remaining.checked_sub(load).expect("loads fit input budget");
            let floor_price = tribute
                .tribute_price_minor
                .max(entry_price)
                .checked_mul(U256::from(108))
                .expect("fixture floor numerator")
                / U256::from(100);
            let cost = entry_price
                .checked_mul(load)
                .expect("fixture cost numerator")
                / scale;
            assert!(!cost.is_zero(), "positive expected cost");
            let mut bucket_preimage = Vec::with_capacity(38);
            bucket_preimage.extend(tribute.worldwide_day.value().to_be_bytes());
            bucket_preimage.extend(floor_price.to_be_bytes::<32>());
            bucket_preimage.extend(tribute.reference_currency.to_be_bytes());
            NodActionV1 {
                raw_ordinal: u32::try_from(ordinal).expect("bounded ordinal"),
                tribute_id: B256::from_slice(tribute.tribute_id.as_slice()),
                // Both entity domains use the same (owner, day) identity. Their
                // storage domain separates Tribute from Nod, not their raw ID.
                nod_id: B256::from_slice(tribute.tribute_id.as_slice()),
                owner: tribute.owner,
                wwd: tribute.worldwide_day.value(),
                league_id: league,
                floor_price_minor: floor_price,
                gratis_load_minor: load,
                entry_price_minor: entry_price,
                cost_amount_minor: cost,
                issuance_currency: tribute.issuance_currency,
                reference_currency: tribute.reference_currency,
                issued_at,
                bucket_key: keccak256(bucket_preimage),
            }
        })
        .collect()
}

fn record(action: &NodActionV1) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RECORD_BYTES);
    bytes.extend(action.raw_ordinal.to_be_bytes());
    bytes.extend(action.tribute_id.as_slice());
    bytes.extend(action.nod_id.as_slice());
    bytes.extend(action.owner.as_slice());
    bytes.extend(action.wwd.to_be_bytes());
    bytes.extend(action.league_id.to_be_bytes());
    bytes.extend(action.floor_price_minor.to_be_bytes::<32>());
    bytes.extend(action.gratis_load_minor.to_be_bytes::<32>());
    bytes.extend(action.entry_price_minor.to_be_bytes::<32>());
    bytes.extend(action.cost_amount_minor.to_be_bytes::<32>());
    bytes.extend(action.issuance_currency.to_be_bytes());
    bytes.extend(action.reference_currency.to_be_bytes());
    bytes.extend(action.issued_at.to_be_bytes());
    bytes.extend(action.bucket_key.as_slice());
    assert_eq!(bytes.len(), RECORD_BYTES, "V1 Nod record width");
    bytes
}

fn framed_hash(domain: &str, pieces: &[&[u8]]) -> B256 {
    let size: usize = pieces.iter().map(|piece| piece.len()).sum();
    let mut bytes = Vec::with_capacity(2 + domain.len() + 4 + size);
    bytes.extend(
        u16::try_from(domain.len())
            .expect("V1 domain length")
            .to_be_bytes(),
    );
    bytes.extend(domain.as_bytes());
    bytes.extend(
        u32::try_from(size)
            .expect("bounded Nod hash payload")
            .to_be_bytes(),
    );
    for piece in pieces {
        bytes.extend(*piece);
    }
    keccak256(bytes)
}

fn subtree(records: &[Vec<u8>], start: u32, height: u16) -> B256 {
    if height == 0 {
        return match records.get(start as usize) {
            Some(bytes) => framed_hash(
                "OUTBE_OCOMP_LIST_LEAF_V1",
                &[
                    &NOD_LIST_KIND,
                    &start.to_be_bytes(),
                    &(RECORD_BYTES as u32).to_be_bytes(),
                    bytes,
                ],
            ),
            None => framed_hash(
                "OUTBE_OCOMP_LIST_PAD_V1",
                &[&NOD_LIST_KIND, &start.to_be_bytes()],
            ),
        };
    }
    let half_width = 1u32 << (height - 1);
    let left = subtree(records, start, height - 1);
    let right = subtree(records, start + half_width, height - 1);
    framed_hash(
        "OUTBE_OCOMP_LIST_NODE_V1",
        &[
            &NOD_LIST_KIND,
            &height.to_be_bytes(),
            &(start >> height).to_be_bytes(),
            left.as_slice(),
            right.as_slice(),
        ],
    )
}

pub(crate) fn nod_root(actions: &[NodActionV1]) -> B256 {
    // Covers the active 1/10-leaf scenarios and the bounded 257-leaf fixture.
    // Avoid an accidentally unbounded reference tree if a fixture changes.
    assert!(actions.len() <= 257, "bounded Nod reference population");
    if actions.is_empty() {
        return framed_hash("OUTBE_OCOMP_LIST_EMPTY_V1", &[&NOD_LIST_KIND]);
    }
    let count = u32::try_from(actions.len()).expect("bounded Nod population");
    let height = count.next_power_of_two().trailing_zeros() as u16;
    let records: Vec<_> = actions.iter().map(record).collect();
    let tree = subtree(&records, 0, height);
    framed_hash(
        "OUTBE_OCOMP_LIST_ROOT_V1",
        &[
            &NOD_LIST_KIND,
            &count.to_be_bytes(),
            &height.to_be_bytes(),
            tree.as_slice(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, U256};
    use outbe_ocomp_protocol::{
        list::{ordered_list_root, OrderedListLimits},
        profile::poc_schema_limits,
        registry::ListKind,
    };

    fn action(ordinal: u32) -> NodActionV1 {
        NodActionV1 {
            raw_ordinal: ordinal,
            tribute_id: B256::from(U256::from(1_000 + ordinal)),
            nod_id: B256::from(U256::from(2_000 + ordinal)),
            owner: Address::repeat_byte((ordinal + 1) as u8),
            wwd: 20_714,
            league_id: 3,
            floor_price_minor: U256::from(1_080_000),
            gratis_load_minor: U256::from(987_654_321),
            entry_price_minor: U256::from(1_000_000),
            cost_amount_minor: U256::from(987_654_321),
            issuance_currency: 949,
            reference_currency: 840,
            issued_at: 1_789_000_123,
            bucket_key: B256::repeat_byte(0xab),
        }
    }

    #[test]
    fn input_arithmetic_preserves_both_load_floors_and_exact_fields() {
        for (count, budget, load, cost) in [
            (1, 2_000_003u64, 2_000_002u64, 2_000_008u64),
            (10, 20_000_019u64, 2_000_000u64, 2_000_006u64),
        ] {
            let inputs: Vec<_> = (0..count)
                .rev()
                .map(|ordinal| {
                    let mut id = [0u8; 32];
                    id[..4].copy_from_slice(&20260908u32.to_be_bytes());
                    id[31] = ordinal as u8;
                    TributeBodyV1 {
                        tribute_id: outbe_compressed_entities::WwdEntityId::from(id),
                        owner: Address::repeat_byte(ordinal as u8 + 1),
                        worldwide_day: outbe_primitives::time::WorldwideDay::new(20260908),
                        issuance_amount_minor: U256::from(2_000_000),
                        issuance_currency: 840,
                        nominal_amount_minor: U256::from(2_000_000),
                        reference_currency: 840,
                        tribute_price_minor: U256::from(1_100_001),
                        exclude_from_intex_issuance: false,
                    }
                })
                .collect();
            let expected = single_league_actions(
                &inputs,
                3,
                U256::from(budget),
                U256::from(1_000_003),
                1_789_000_123,
            );
            for (ordinal, action) in expected.iter().enumerate() {
                assert_eq!(action.raw_ordinal, ordinal as u32);
                assert_eq!(action.tribute_id[31], ordinal as u8);
                assert_eq!(action.nod_id, action.tribute_id);
                assert_eq!(action.owner, Address::repeat_byte(ordinal as u8 + 1));
                assert_eq!(action.wwd, 20260908);
                assert_eq!(action.league_id, 3);
                assert_eq!(action.floor_price_minor, U256::from(1_188_001));
                assert_eq!(action.gratis_load_minor, U256::from(load));
                assert_eq!(action.entry_price_minor, U256::from(1_000_003));
                assert_eq!(action.cost_amount_minor, U256::from(cost));
                assert_eq!(
                    (action.issuance_currency, action.reference_currency),
                    (840, 840)
                );
                assert_eq!(action.issued_at, 1_789_000_123);
                assert_eq!(action.bucket_key, expected[0].bucket_key);
            }
        }
    }

    #[test]
    fn independent_encoding_and_root_match_v1_for_one_and_ten_actions() {
        for count in [1, 10] {
            let actions: Vec<_> = (0..count).map(action).collect();
            let production_records: Vec<_> = actions
                .iter()
                .map(|value| value.encode_canonical_record(&poc_schema_limits()).unwrap())
                .collect();
            for (value, encoded) in actions.iter().zip(&production_records) {
                assert_eq!(record(value), *encoded);
            }
            assert_eq!(
                nod_root(&actions),
                ordered_list_root(
                    ListKind::NodActions,
                    &production_records,
                    OrderedListLimits::new(257, RECORD_BYTES, 16_384),
                )
                .unwrap()
            );
        }
    }

    #[test]
    fn every_field_order_population_and_padding_are_bound() {
        let actions: Vec<_> = (0..10).map(action).collect();
        let expected = nod_root(&actions);
        for field in 0..14 {
            let mut changed = actions.clone();
            let last = &mut changed[9];
            match field {
                0 => last.raw_ordinal += 1,
                1 => last.tribute_id = B256::ZERO,
                2 => last.nod_id = B256::ZERO,
                3 => last.owner = Address::ZERO,
                4 => last.wwd += 1,
                5 => last.league_id += 1,
                6 => last.floor_price_minor += U256::from(1),
                7 => last.gratis_load_minor += U256::from(1),
                8 => last.entry_price_minor += U256::from(1),
                9 => last.cost_amount_minor += U256::from(1),
                10 => last.issuance_currency += 1,
                11 => last.reference_currency += 1,
                12 => last.issued_at += 1,
                13 => last.bucket_key = B256::ZERO,
                _ => unreachable!(),
            }
            assert_ne!(nod_root(&changed), expected, "unbound field {field}");
        }
        let mut reordered = actions.clone();
        reordered.swap(0, 1);
        assert_ne!(nod_root(&reordered), expected);
        assert_ne!(nod_root(&actions[..9]), expected);
        // Filling the six padding positions with real records must also differ.
        let padded_with_real: Vec<_> = (0..16).map(action).collect();
        assert_ne!(nod_root(&padded_with_real), expected);
    }
}
