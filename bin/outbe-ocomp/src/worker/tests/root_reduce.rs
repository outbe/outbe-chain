use super::*;

#[test]
fn root_reduce_rejects_finalized_payload_that_no_longer_matches_coverage_header() {
    let day = WorldwideDay::new(20_260_725);
    let owner = Address::repeat_byte(0x11);
    let tribute_id = derive_poseidon_entity_id(owner, day).unwrap();
    let finalized = output_finalize(
        &AmountRunV1 {
            start_ordinal: 0,
            end_ordinal: 1,
            ordered_records: vec![AmountRecordV1 {
                raw_ordinal: 0,
                tribute_id,
                owner,
                worldwide_day: day,
                league_id: 1,
                nominal_amount_minor: U256::from(10),
                gratis_fraction_fp: U256::ZERO,
                gratis_load_minor: U256::from(1),
                entry_price_minor: U256::from(2),
                floor_price_minor: U256::from(3),
                cost_amount_minor: U256::from(4),
                issuance_currency: 840,
                reference_currency: 978,
                exclude_from_intex_issuance: false,
            }],
            checked_segment_gratis_total: U256::from(1),
        },
        &GratisLeafPrefixV1 {
            segment_ordinal: 0,
            incoming_remaining: U256::from(2),
            outgoing_remaining: U256::from(1),
            first_error_ordinal: None,
        },
        2_026_072_500,
    )
    .unwrap();
    let coverage_root = finalized.coverage_root().unwrap();
    let header = WorkOutputHeaderV1 {
        source_coverage_root: coverage_root,
        output_coverage_root: coverage_root,
        source_coverage_count: 1,
        output_coverage_count: 1,
    };
    require_root_reduce_finalized_binding(&finalized, header.clone(), 0, 1, 1).unwrap();

    let mut changed = finalized;
    changed.ordered_records[0].nod_action.source_tribute_id =
        derive_poseidon_entity_id(Address::repeat_byte(0x12), day).unwrap();
    assert!(require_root_reduce_finalized_binding(&changed, header, 0, 1, 1).is_err());
}

#[test]
fn root_reduce_rejects_hidden_shuffle_suffix_or_missing_bucket_record() {
    let source_root = B256::repeat_byte(0x21);
    require_root_reduce_shuffle_population(256, 256, source_root, source_root, 255, 256, 256)
        .unwrap();
    assert!(require_root_reduce_shuffle_population(
        256,
        256,
        source_root,
        source_root,
        257,
        256,
        256,
    )
    .is_err());
    assert!(require_root_reduce_shuffle_population(
        256,
        256,
        source_root,
        source_root,
        255,
        255,
        256,
    )
    .is_err());
}

#[test]
fn complete_root_rejects_manifest_total_mismatch_for_leaf_and_node() {
    for primary_work_unit_count in [1, 2] {
        assert!(require_complete_root_values(
            0,
            primary_work_unit_count,
            primary_work_unit_count,
            257,
            257,
            257,
            U256::from(256),
            U256::from(200),
            U256::from(257),
        )
        .is_err());
    }
    require_complete_root_values(
        0,
        1,
        2,
        256,
        257,
        257,
        U256::from(256),
        U256::from(200),
        U256::from(257),
    )
    .unwrap();
    assert!(require_complete_root_values(
        0,
        2,
        2,
        257,
        257,
        257,
        U256::from(257),
        U256::from(258),
        U256::from(257),
    )
    .is_err());
}
