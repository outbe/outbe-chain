use super::*;

#[test]
fn an_entry_beyond_the_call_price_bound_is_never_certified() {
    let day = WorldwideDay::new(20_260_724);
    let entry = U256::MAX / U256::from(100 + u32::from(u16::MAX)) + U256::from(1);
    assert!(outbe_nod::NodContract::floor_price_minor(entry).is_some());
    let mut item = observed(1, day, 100, 1, false);
    item.entry_price_minor = ObservationValueV1::Value(entry);
    let tributes = vec![item];
    let budget = U256::from(100);
    assert!(matches!(
        execute(ProgramInputV1 {
            worldwide_day: day,
            logical_evaluation_time: 1_784_765_900,
            lysis_limit_minor: budget,
            tributes: tributes.clone(),
        }),
        Err(ProgramErrorV1::Arithmetic { .. })
    ));
    let fidelity = fidelity_map(0, &tributes).unwrap();
    let fractions = finalize_fi_fraction_table(&fidelity.aggregate, budget).unwrap();
    assert!(matches!(
        amount_map(0, &tributes, &fidelity.observations, &fractions),
        Err(ProgramErrorV1::Arithmetic { .. })
    ));

    let owner = Address::repeat_byte(1);
    let amount = AmountRunV1 {
        start_ordinal: 0,
        end_ordinal: 1,
        ordered_records: vec![AmountRecordV1 {
            raw_ordinal: 0,
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            league_id: 1,
            nominal_amount_minor: U256::from(100),
            gratis_fraction_fp: SIX_DECIMAL_SCALE,
            gratis_load_minor: U256::from(1),
            entry_price_minor: entry,
            settlement_cost_minor: U256::from(1),
            issuance_currency: 840,
            reference_currency: 978,
            exclude_from_intex_issuance: false,
        }],
        checked_segment_gratis_total: U256::from(1),
    };
    let limits = poc_schema_limits();
    assert!(matches!(
        encode_amount_run(&amount, &limits),
        Err(LysisArtifactErrorV1::InvalidEncoding(
            "amount run Nod entry price bound"
        ))
    ));
    let finalized = output_finalize(
        &amount,
        &GratisLeafPrefixV1 {
            segment_ordinal: 0,
            incoming_remaining: U256::from(2),
            outgoing_remaining: U256::from(1),
            first_error_ordinal: None,
        },
    )
    .unwrap();
    assert!(encode_finalized_output_run(&finalized, &limits).is_err());
}

#[test]
fn fidelity_reducer_handles_every_padded_empty_shape_for_one_to_eight_shards() {
    let day = WorldwideDay::new(20_260_724);
    for primary_count in 1..=8_u32 {
        let tree = PaddedBinaryTreeV1::for_primary_leaf_count(primary_count).unwrap();
        let leaves = (0..primary_count)
            .map(|ordinal| {
                let mut owner_bytes = [0_u8; 20];
                owner_bytes[16..].copy_from_slice(&(ordinal + 1).to_be_bytes());
                let owner = Address::from(owner_bytes);
                let observed = ObservedTributeV1 {
                    tribute: TributeInputV1 {
                        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                        owner,
                        worldwide_day: day,
                        issuance_currency: 978,
                        nominal_amount_minor: SIX_DECIMAL_SCALE,
                        reference_currency: 840,
                        tribute_price_minor: U256::ZERO,
                        exclude_from_intex_issuance: false,
                    },
                    first_league: ObservationValueV1::Value((ordinal % 3 + 1) as u16),
                    second_league: ObservationValueV1::Value((ordinal % 3 + 1) as u16),
                    entry_price_minor: ObservationValueV1::Value(SIX_DECIMAL_SCALE),
                    nod_target_available: true,
                };
                fidelity_map(ordinal, &[observed]).unwrap().aggregate
            })
            .collect::<Vec<_>>();
        let mut nodes = BTreeMap::<(u16, u32), FidelityReduceValueV1>::new();

        for level in 1..=tree.height() {
            let width = tree.padded_leaf_count() >> level;
            for index in 0..width {
                let node = tree.reducer_node(level, index).unwrap();
                let resolve = |input| match input {
                    ReducerInputV1::Primary(ordinal) => {
                        FidelityReduceValueV1::Aggregate(leaves[ordinal as usize].clone())
                    }
                    ReducerInputV1::CanonicalEmpty { .. } => FidelityReduceValueV1::Empty,
                    ReducerInputV1::Reducer { level, index } => {
                        nodes.get(&(level, index)).unwrap().clone()
                    }
                };
                let output =
                    fidelity_reduce_pair(resolve(node.inputs[0]), resolve(node.inputs[1])).unwrap();
                nodes.insert((level, index), output);
            }
        }

        let FidelityReduceValueV1::Aggregate(root) = nodes.get(&(tree.height(), 0)).unwrap() else {
            panic!("a non-empty plan must have a non-empty Fidelity root");
        };
        assert_eq!(root.tribute_count, primary_count);
        assert_eq!(root.start_ordinal, 0);
        assert_eq!(root.end_ordinal, primary_count);
    }
}

#[test]
fn two_direction_gratis_prefix_preserves_exact_sequential_budget_semantics() {
    let first_loads = vec![U256::from(1_u8); 256];
    let second_loads = vec![U256::from(1_u8)];
    let first = gratis_summary(0, &first_loads).unwrap();
    let second = gratis_summary(256, &second_loads).unwrap();
    let root = gratis_summary_reduce_pair(
        GratisSummaryValueV1::Summary(first.clone()),
        GratisSummaryValueV1::Summary(second.clone()),
    )
    .unwrap();
    let GratisSummaryValueV1::Summary(root) = root else {
        panic!("non-empty Gratis tree has a summary");
    };
    assert_eq!(root.checked_segment_gratis_total, U256::from(257_u16));

    let exact = gratis_prefix_down(
        Some(U256::from(257_u16)),
        GratisSummaryValueV1::Summary(first.clone()),
        GratisSummaryValueV1::Summary(second.clone()),
    )
    .unwrap();
    assert_eq!(
        exact[0].as_ref().unwrap().incoming_remaining,
        Some(U256::from(257_u16))
    );
    assert_eq!(
        exact[1].as_ref().unwrap().incoming_remaining,
        Some(U256::from(1_u8))
    );
    let first_leaf = finalize_gratis_leaf(
        exact[0].as_ref().unwrap().incoming_remaining,
        0,
        &first_loads,
    )
    .unwrap();
    let second_leaf = finalize_gratis_leaf(
        exact[1].as_ref().unwrap().incoming_remaining,
        256,
        &second_loads,
    )
    .unwrap();
    assert_eq!(first_leaf.outgoing_remaining, U256::from(1_u8));
    assert_eq!(second_leaf.outgoing_remaining, U256::ZERO);
    assert_eq!(second_leaf.first_error_ordinal, None);

    let exhausted = gratis_prefix_down(
        Some(U256::from(256_u16)),
        GratisSummaryValueV1::Summary(first),
        GratisSummaryValueV1::Summary(second),
    )
    .unwrap();
    let error = finalize_gratis_leaf(
        exhausted[1].as_ref().unwrap().incoming_remaining,
        256,
        &second_loads,
    )
    .unwrap_err();
    assert_eq!(
        error,
        outbe_lysis::program_v1::ProgramErrorV1::GratisLoadExceedsRemaining { ordinal: 256 }
    );
}

#[test]
fn gratis_prefix_treats_padding_as_identity_not_as_work() {
    let summary = gratis_summary(0, &[U256::from(9_u8)]).unwrap();
    let reduced = gratis_summary_reduce_pair(
        GratisSummaryValueV1::Summary(summary.clone()),
        GratisSummaryValueV1::Empty,
    )
    .unwrap();
    assert_eq!(reduced, GratisSummaryValueV1::Summary(summary.clone()));

    let children = gratis_prefix_down(
        Some(U256::from(10_u8)),
        GratisSummaryValueV1::Summary(summary),
        GratisSummaryValueV1::Empty,
    )
    .unwrap();
    assert_eq!(children.len(), 2);
    assert!(children[0].is_some());
    assert!(children[1].is_none());
}
