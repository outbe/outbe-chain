use super::*;

#[test]
fn shard_cap_plus_one_places_the_last_tribute_in_the_second_adjacent_range() {
    let tree = PaddedBinaryTreeV1::for_tribute_count(257).unwrap();
    assert_eq!(tree.primary_leaf_count(), 2);

    let first = tree.primary_shard(0).unwrap();
    let second = tree.primary_shard(1).unwrap();
    assert_eq!((first.start_ordinal, first.end_ordinal), (0, 256));
    assert_eq!((second.start_ordinal, second.end_ordinal), (256, 257));
    assert_eq!(first.end_ordinal, second.start_ordinal);
    assert_eq!(second.record_count(), 1);
    assert_eq!(
        tree.primary_shard(2),
        Err(PlannerErrorV1::PrimaryShardOutOfRange {
            ordinal: 2,
            primary_leaf_count: 2,
        })
    );
}

#[test]
fn padded_binary_reducer_is_derived_by_position_without_completion_order() {
    let tree = PaddedBinaryTreeV1::for_primary_leaf_count(3).unwrap();
    assert_eq!(tree.padded_leaf_count(), 4);
    assert_eq!(tree.height(), 2);
    assert_eq!(tree.reducer_node_count(), 3);

    let left = tree.reducer_node(1, 0).unwrap();
    assert_eq!(
        left.inputs,
        [ReducerInputV1::Primary(0), ReducerInputV1::Primary(1)]
    );

    let padded = tree.reducer_node(1, 1).unwrap();
    assert_eq!(
        padded.inputs,
        [
            ReducerInputV1::Primary(2),
            ReducerInputV1::CanonicalEmpty { padded_ordinal: 3 },
        ]
    );

    let root = tree.reducer_node(2, 0).unwrap();
    assert_eq!(
        root.inputs,
        [
            ReducerInputV1::Reducer { level: 1, index: 0 },
            ReducerInputV1::Reducer { level: 1, index: 1 },
        ]
    );

    let single = PaddedBinaryTreeV1::for_primary_leaf_count(1).unwrap();
    assert_eq!(single.padded_leaf_count(), 2);
    assert_eq!(single.height(), 1);
    assert_eq!(
        single.reducer_node(1, 0).unwrap().inputs,
        [
            ReducerInputV1::Primary(0),
            ReducerInputV1::CanonicalEmpty { padded_ordinal: 1 },
        ]
    );
}

#[test]
fn primary_catalog_and_units_are_deterministic_and_lazily_derived() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(257)).unwrap();
    let ids = (0..257).map(entity_id).collect::<Vec<_>>();
    let chunks = [
        tribute_chunk_ref(0, &ids[..256], 65_536),
        tribute_chunk_ref(1, &ids[256..], 512),
    ];

    let mut lookups = Vec::new();
    let first = planner
        .primary_unit_at(
            0,
            |ordinal| {
                lookups.push(ordinal);
                chunks.get(ordinal as usize).cloned()
            },
            &limits,
        )
        .unwrap();
    assert_eq!(lookups, [0, 1]);
    assert_eq!(first.phase, UnitPhase::Enumerate);
    assert_eq!(
        first
            .canonical_ordered_inputs
            .iter()
            .map(|input| (input.purpose, input.source_kind))
            .collect::<Vec<_>>(),
        [
            (
                InputPurpose::InputManifest,
                InputSourceKind::AuthenticatedRoot
            ),
            (
                InputPurpose::TributeStream,
                InputSourceKind::AuthenticatedRoot
            ),
        ]
    );
    assert_eq!(
        first.interval,
        UnitInterval::EntityIdRange(outbe_ocomp_protocol::unit::EntityIdHalfOpenRange {
            start: ids[0],
            end: Some(ids[256]),
        })
    );
    assert_eq!(
        first.canonical_ordered_inputs[1].source_id,
        chunks[0].semantic_digest
    );
    assert_eq!(
        first.canonical_ordered_inputs[1].max_encoded_bytes,
        chunks[0].encoded_bytes
    );

    lookups.clear();
    let second = planner
        .primary_unit_at(
            1,
            |ordinal| {
                lookups.push(ordinal);
                chunks.get(ordinal as usize).cloned()
            },
            &limits,
        )
        .unwrap();
    assert_eq!(lookups, [1]);
    assert_eq!(
        second.interval,
        UnitInterval::EntityIdRange(outbe_ocomp_protocol::unit::EntityIdHalfOpenRange {
            start: ids[256],
            end: None,
        })
    );
    assert_eq!(
        second.canonical_ordered_inputs[1].source_id,
        chunks[1].semantic_digest
    );

    let plan = planner
        .commit_primary_catalog(chunks.iter().cloned(), &limits)
        .unwrap();
    let replay = planner
        .commit_primary_catalog(chunks.iter().cloned(), &limits)
        .unwrap();
    assert_eq!(plan, replay);
    assert_eq!(plan.primary_work_unit_count, 2);
    assert_eq!(plan.tribute_count, 257);
    assert_eq!(plan.wwd, 20_260_724);
    assert_eq!(plan.lysis_limit_minor, U256::from(99_000_000_u64));
    assert_eq!(plan.logical_evaluation_time, 1_784_765_900);
    assert_eq!(
        plan.plan_hash(&limits).unwrap(),
        replay.plan_hash(&limits).unwrap()
    );

    let mut wrong_chunk = chunks[0].clone();
    wrong_chunk.first_key.0[0] ^= 1;
    assert!(planner
        .primary_unit_at(
            0,
            |ordinal| {
                if ordinal == 0 {
                    Some(wrong_chunk.clone())
                } else {
                    chunks.get(ordinal as usize).cloned()
                }
            },
            &limits,
        )
        .is_err());
    assert!(planner
        .commit_primary_catalog(std::iter::once(chunks[0].clone()), &limits)
        .is_err());
    assert!(planner
        .commit_primary_catalog(
            chunks
                .iter()
                .cloned()
                .chain(std::iter::once(chunks[1].clone())),
            &limits,
        )
        .is_err());

    let mut changed_budget = plan.clone();
    changed_budget.lysis_limit_minor += U256::from(1);
    assert_ne!(
        changed_budget.plan_hash(&limits).unwrap(),
        plan.plan_hash(&limits).unwrap()
    );
}

#[test]
fn fidelity_map_unit_is_derived_only_from_plan_and_exact_enumerate_unit() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(257)).unwrap();
    let enumerate_unit_id = B256::repeat_byte(0x31);

    let first = planner
        .fidelity_map_unit_at(0, enumerate_unit_id, &limits)
        .unwrap();
    assert_eq!(first.phase, UnitPhase::FidelityMap);
    assert_eq!(
        first.interval,
        UnitInterval::FidelityIndexRange(outbe_ocomp_protocol::unit::FidelityIndexHalfOpenRange {
            start: 0,
            end: 256,
        })
    );
    assert_eq!(
        first
            .canonical_ordered_inputs
            .iter()
            .map(|input| (input.purpose, input.source_kind, input.source_id))
            .collect::<Vec<_>>(),
        [
            (
                InputPurpose::InputManifest,
                InputSourceKind::AuthenticatedRoot,
                planner_bindings(257).input_manifest_hash,
            ),
            (
                InputPurpose::EnumeratedTributes,
                InputSourceKind::UnitOutput,
                enumerate_unit_id,
            ),
            (
                InputPurpose::FidelityOpenings,
                InputSourceKind::AuthenticatedRoot,
                planner_bindings(257).fidelity_opening_root,
            ),
        ]
    );

    let second = planner
        .fidelity_map_unit_at(1, B256::repeat_byte(0x32), &limits)
        .unwrap();
    assert_eq!(
        second.interval,
        UnitInterval::FidelityIndexRange(outbe_ocomp_protocol::unit::FidelityIndexHalfOpenRange {
            start: 256,
            end: 257,
        })
    );
    assert_ne!(
        first.unit_id(&limits).unwrap(),
        planner
            .fidelity_map_unit_at(0, B256::repeat_byte(0x33), &limits)
            .unwrap()
            .unit_id(&limits)
            .unwrap()
    );
    assert!(planner
        .fidelity_map_unit_at(0, B256::ZERO, &limits)
        .is_err());
}

#[test]
fn fixed_reduce_units_bind_exact_left_right_producers_and_canonical_padding() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(513)).unwrap();
    let left = B256::repeat_byte(0x41);
    let right = B256::repeat_byte(0x42);

    let first = planner
        .fixed_reduce_unit_at(0, [Some(left), Some(right)], &limits)
        .unwrap();
    assert_eq!(first.phase, UnitPhase::FixedReduce);
    assert_eq!(
        first.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 1,
            index: 0
        })
    );
    assert_eq!(
        first
            .canonical_ordered_inputs
            .iter()
            .skip(1)
            .map(|input| (input.source_kind, input.source_id))
            .collect::<Vec<_>>(),
        [
            (InputSourceKind::UnitOutput, left),
            (InputSourceKind::UnitOutput, right),
        ]
    );

    let padded = planner
        .fixed_reduce_unit_at(1, [Some(left), None], &limits)
        .unwrap();
    assert_eq!(
        padded.canonical_ordered_inputs[2].source_kind,
        InputSourceKind::CanonicalEmpty
    );
    assert!(planner
        .fixed_reduce_unit_at(0, [Some(left), None], &limits)
        .is_err());
    assert!(planner
        .fixed_reduce_unit_at(1, [Some(left), Some(right)], &limits)
        .is_err());
}

#[test]
fn amount_map_unit_binds_exact_enumerate_fidelity_root_and_oracle_inputs() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(257)).unwrap();
    let ids = (0..257).map(entity_id).collect::<Vec<_>>();
    let chunks = [
        tribute_chunk_ref(0, &ids[..256], 10_000),
        tribute_chunk_ref(1, &ids[256..], 100),
    ];
    let enumerate = planner
        .primary_unit_at(1, |ordinal| chunks.get(ordinal as usize).cloned(), &limits)
        .unwrap();
    let enumerate_id = enumerate.unit_id(&limits).unwrap();
    let fidelity_id = B256::repeat_byte(0x51);
    let root_id = B256::repeat_byte(0x52);
    let amount = planner
        .amount_map_unit_at(1, &enumerate, fidelity_id, root_id, &limits)
        .unwrap();

    assert_eq!(amount.phase, UnitPhase::AmountMap);
    assert_eq!(amount.interval, enumerate.interval);
    assert_eq!(
        amount
            .canonical_ordered_inputs
            .iter()
            .map(|input| (input.purpose, input.source_kind, input.source_id))
            .collect::<Vec<_>>(),
        [
            (
                InputPurpose::InputManifest,
                InputSourceKind::AuthenticatedRoot,
                planner_bindings(257).input_manifest_hash,
            ),
            (
                InputPurpose::EnumeratedTributes,
                InputSourceKind::UnitOutput,
                enumerate_id,
            ),
            (
                InputPurpose::FidelityPartials,
                InputSourceKind::UnitOutput,
                fidelity_id,
            ),
            (
                InputPurpose::FiFractionTable,
                InputSourceKind::UnitOutput,
                root_id,
            ),
            (
                InputPurpose::OracleOpenings,
                InputSourceKind::AuthenticatedRoot,
                planner_bindings(257).oracle_opening_root,
            ),
        ]
    );
}

#[test]
fn gratis_prefix_units_bind_exact_amount_children_and_padding() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(513)).unwrap();
    let left = B256::repeat_byte(0x61);
    let right = B256::repeat_byte(0x62);

    let leaf = planner
        .gratis_prefix_unit_at(0, &[Some(left)], &limits)
        .unwrap();
    assert_eq!(leaf.phase, UnitPhase::GratisPrefix);
    assert_eq!(
        leaf.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 0,
            index: 0
        })
    );
    assert_eq!(
        leaf.canonical_ordered_inputs[1].purpose,
        InputPurpose::AmountRecords
    );
    assert_eq!(leaf.canonical_ordered_inputs[1].source_id, left);

    let internal = planner
        .gratis_prefix_unit_at(3, &[Some(left), Some(right)], &limits)
        .unwrap();
    assert_eq!(
        internal.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 1,
            index: 0
        })
    );
    assert!(internal
        .canonical_ordered_inputs
        .iter()
        .skip(1)
        .all(|input| input.purpose == InputPurpose::GratisPrefixTable));

    let padded = planner
        .gratis_prefix_unit_at(4, &[Some(left), None], &limits)
        .unwrap();
    assert_eq!(
        padded.canonical_ordered_inputs[2].source_kind,
        InputSourceKind::CanonicalEmpty
    );
    assert!(planner
        .gratis_prefix_unit_at(0, &[Some(left), Some(right)], &limits)
        .is_err());
    assert!(planner
        .gratis_prefix_unit_at(4, &[Some(left), Some(right)], &limits)
        .is_err());
}

#[test]
fn gratis_prefix_down_units_bind_parent_and_immediate_summaries() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(513)).unwrap();
    let parent = B256::repeat_byte(0x71);
    let left = B256::repeat_byte(0x72);
    let right = B256::repeat_byte(0x73);

    let root = planner
        .gratis_prefix_down_unit_at(0, &[Some(left), Some(right)], &limits)
        .unwrap();
    assert_eq!(root.phase, UnitPhase::GratisPrefixDown);
    assert_eq!(
        root.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 2,
            index: 0
        })
    );

    let internal = planner
        .gratis_prefix_down_unit_at(2, &[Some(parent), Some(left), None], &limits)
        .unwrap();
    assert_eq!(
        internal.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 1,
            index: 1
        })
    );
    assert_eq!(internal.canonical_ordered_inputs[1].source_id, parent);
    assert_eq!(
        internal.canonical_ordered_inputs[3].source_kind,
        InputSourceKind::CanonicalEmpty
    );

    let leaf = planner
        .gratis_prefix_down_unit_at(5, &[Some(parent), Some(left)], &limits)
        .unwrap();
    assert_eq!(
        leaf.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 0,
            index: 2
        })
    );
    assert!(leaf
        .canonical_ordered_inputs
        .iter()
        .skip(1)
        .all(|input| input.purpose == InputPurpose::GratisPrefixTable));
    assert!(planner
        .gratis_prefix_down_unit_at(0, &[Some(parent), Some(left), Some(right)], &limits)
        .is_err());
}

#[test]
fn output_finalize_unit_binds_exact_amount_and_leaf_prefix() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(257)).unwrap();
    let ids = (0..257).map(entity_id).collect::<Vec<_>>();
    let chunks = [
        tribute_chunk_ref(0, &ids[..256], 10_000),
        tribute_chunk_ref(1, &ids[256..], 100),
    ];
    let enumerate = planner
        .primary_unit_at(1, |ordinal| chunks.get(ordinal as usize).cloned(), &limits)
        .unwrap();
    let amount = planner
        .amount_map_unit_at(
            1,
            &enumerate,
            B256::repeat_byte(0x91),
            B256::repeat_byte(0x92),
            &limits,
        )
        .unwrap();
    let prefix_id = B256::repeat_byte(0x93);
    let finalize = planner
        .output_finalize_unit_at(1, &amount, prefix_id, &limits)
        .unwrap();

    assert_eq!(finalize.phase, UnitPhase::OutputFinalize);
    assert_eq!(finalize.interval, amount.interval);
    assert_eq!(
        finalize
            .canonical_ordered_inputs
            .iter()
            .skip(1)
            .map(|input| (input.purpose, input.source_kind, input.source_id))
            .collect::<Vec<_>>(),
        [
            (
                InputPurpose::AmountRecords,
                InputSourceKind::UnitOutput,
                amount.unit_id(&limits).unwrap(),
            ),
            (
                InputPurpose::GratisPrefixTable,
                InputSourceKind::UnitOutput,
                prefix_id,
            ),
        ]
    );
    assert!(planner
        .output_finalize_unit_at(1, &amount, B256::ZERO, &limits)
        .is_err());
    let mut wrong_phase = amount;
    wrong_phase.phase = UnitPhase::Enumerate;
    assert!(planner
        .output_finalize_unit_at(1, &wrong_phase, prefix_id, &limits)
        .is_err());
}

#[test]
fn shuffle_units_bind_only_the_exact_materialized_producer_runs() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(513)).unwrap();
    let leaf_source = B256::repeat_byte(0x71);
    let leaf = planner
        .shuffle_unit_at(UnitPhase::OwnerShuffle, 2, &[leaf_source], &limits)
        .unwrap();
    assert_eq!(
        leaf.interval,
        UnitInterval::CanonicalRunSpan(outbe_ocomp_protocol::unit::CanonicalRunSpan {
            start_run: 2,
            end_run: 3,
        })
    );
    assert_eq!(
        leaf.canonical_ordered_inputs[1].purpose,
        InputPurpose::FinalizedOutputRecords
    );
    assert_eq!(leaf.canonical_ordered_inputs[1].source_id, leaf_source);

    let left = B256::repeat_byte(0x72);
    let right = B256::repeat_byte(0x73);
    let root = planner
        .shuffle_unit_at(UnitPhase::OwnerShuffle, 4, &[left, right], &limits)
        .unwrap();
    assert_eq!(
        root.interval,
        UnitInterval::CanonicalRunSpan(outbe_ocomp_protocol::unit::CanonicalRunSpan {
            start_run: 0,
            end_run: 3,
        })
    );
    assert_eq!(
        root.canonical_ordered_inputs[1..]
            .iter()
            .map(|input| (input.purpose, input.source_kind, input.source_id))
            .collect::<Vec<_>>(),
        [
            (
                InputPurpose::OwnerOrderedRecords,
                InputSourceKind::UnitOutput,
                left,
            ),
            (
                InputPurpose::OwnerOrderedRecords,
                InputSourceKind::UnitOutput,
                right,
            ),
        ]
    );
    assert!(planner
        .shuffle_unit_at(UnitPhase::OwnerShuffle, 4, &[left], &limits)
        .is_err());
    assert!(planner
        .shuffle_unit_at(UnitPhase::BucketShuffle, 4, &[left, B256::ZERO], &limits,)
        .is_err());
    assert!(planner
        .shuffle_unit_at(UnitPhase::RootReduce, 4, &[left, right], &limits)
        .is_err());
}

#[test]
fn root_reduce_units_bind_exact_leaf_shuffle_roots_and_padded_summaries() {
    let limits = poc_schema_limits();
    let planner = LysisPlannerV1::new(planner_bindings(513)).unwrap();
    let finalized = B256::repeat_byte(0x81);
    let owner_root = B256::repeat_byte(0x82);
    let bucket_root = B256::repeat_byte(0x83);

    let leaf = planner
        .root_reduce_unit_at(
            2,
            &[Some(finalized), Some(owner_root), Some(bucket_root)],
            &limits,
        )
        .unwrap();
    assert_eq!(leaf.phase, UnitPhase::RootReduce);
    assert_eq!(
        leaf.interval,
        UnitInterval::BinaryReducerNode(outbe_ocomp_protocol::unit::BinaryReducerNode {
            level: 0,
            index: 2
        })
    );
    assert_eq!(
        leaf.canonical_ordered_inputs[1..]
            .iter()
            .map(|input| (input.purpose, input.source_kind, input.source_id))
            .collect::<Vec<_>>(),
        [
            (
                InputPurpose::FinalizedOutputRecords,
                InputSourceKind::UnitOutput,
                finalized,
            ),
            (
                InputPurpose::OwnerOrderedRecords,
                InputSourceKind::UnitOutput,
                owner_root,
            ),
            (
                InputPurpose::BucketOrderedRecords,
                InputSourceKind::UnitOutput,
                bucket_root,
            ),
        ]
    );

    let left = B256::repeat_byte(0x84);
    let right = B256::repeat_byte(0x85);
    let internal = planner
        .root_reduce_unit_at(3, &[Some(left), Some(right)], &limits)
        .unwrap();
    assert!(internal
        .canonical_ordered_inputs
        .iter()
        .skip(1)
        .all(|input| input.purpose == InputPurpose::RootSummary));

    let padded = planner
        .root_reduce_unit_at(4, &[Some(left), None], &limits)
        .unwrap();
    assert_eq!(
        padded.canonical_ordered_inputs[2].source_kind,
        InputSourceKind::CanonicalEmpty
    );
    assert!(planner
        .root_reduce_unit_at(2, &[Some(finalized), Some(owner_root)], &limits)
        .is_err());
    assert!(planner
        .root_reduce_unit_at(3, &[Some(left), Some(B256::ZERO)], &limits)
        .is_err());
}

#[test]
fn one_shard_root_reduce_finishes_at_the_leaf_without_a_padded_node() {
    let topology = LysisPlanTopologyV1::new(1).unwrap();
    let final_position = PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::RootReduce,
        level: 0,
        index: 0,
    };

    assert_eq!(topology.phase_unit_count(UnitPhase::RootReduce), 1);
    assert_eq!(
        topology
            .phase_position_at(UnitPhase::RootReduce, 0)
            .unwrap(),
        final_position
    );
    assert_eq!(
        topology
            .plan_position_at(topology.plan_ordinal_of(final_position).unwrap())
            .unwrap(),
        final_position
    );
    assert!(topology
        .phase_position_at(UnitPhase::RootReduce, 1)
        .is_err());
}
