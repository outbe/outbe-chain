use outbe_lysis::program_v1::planner::{
    primary_work_unit_count, LysisPlannerBindingsV1, LysisPlannerV1, PaddedBinaryTreeV1,
    LysisPlanTopologyV1, PlannedProducerV1, PlannedUnitPositionV1, PlannerErrorV1,
    ReducerInputV1, PRIMARY_WORK_SHARD_SIZE,
};
use outbe_lysis::program_v1::phases::{
    amount_map, fidelity_map, fidelity_reduce, fidelity_reduce_pair, finalize_fi_fraction_table,
    finalize_gratis_leaf, gratis_prefix_down, gratis_summary, gratis_summary_reduce_pair,
    output_finalize, FidelityReduceValueV1, GratisSummaryValueV1,
};
use outbe_lysis::program_v1::{
    execute, ObservationValueV1, ObservedTributeV1, ProgramInputV1, TributeInputV1,
};
use outbe_ocomp_protocol::{
    common::EntityId36,
    local_control::poc_schema_limits,
    unit::{InputPurpose, InputSourceKind, UnitInterval, UnitPhase},
};
use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::derive_poseidon_entity_id;
use outbe_primitives::units::SCALE_1E18;
use std::collections::BTreeMap;

// OCM-SEM-002: planner, unit coverage and fixed reducer topology.
#[test]
fn primary_work_is_unbounded_in_total_and_partitioned_in_exact_256_record_shards() {
    assert_eq!(PRIMARY_WORK_SHARD_SIZE, 256);
    for (tribute_count, expected_units) in [
        (1, 1),
        (255, 1),
        (256, 1),
        (257, 2),
        (10_000, 40),
        (1_000_000_000, 3_906_250),
    ] {
        assert_eq!(
            primary_work_unit_count(tribute_count).unwrap(),
            expected_units,
            "unexpected primary unit count for {tribute_count} Tribute"
        );
    }
    assert_eq!(
        primary_work_unit_count(0),
        Err(PlannerErrorV1::EmptyTributePopulation)
    );
}

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
    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 3,
        input_manifest_hash: B256::repeat_byte(4),
        input_manifest_encoded_bytes: 512,
        tribute_collection_root: B256::repeat_byte(5),
        tribute_input_encoded_bytes: 65_536,
        fidelity_opening_root: B256::repeat_byte(6),
        oracle_opening_root: B256::repeat_byte(7),
        wwd: 20_260_724,
        lysis_budget: U256::from(99_000_000_u64),
        logical_evaluation_time: 1_784_765_900,
        tribute_count: 257,
        lysis_program_semantics_hash: B256::repeat_byte(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
    })
    .unwrap();
    let ids = (0..257).map(entity_id).collect::<Vec<_>>();

    let mut lookups = Vec::new();
    let first = planner
        .primary_unit_at(
            0,
            |ordinal| {
                lookups.push(ordinal);
                ids.get(ordinal as usize).copied()
            },
            &limits,
        )
        .unwrap();
    assert_eq!(lookups, [0, 256]);
    assert_eq!(first.phase, UnitPhase::Enumerate);
    assert_eq!(
        first
            .canonical_ordered_inputs
            .iter()
            .map(|input| (input.purpose, input.source_kind))
            .collect::<Vec<_>>(),
        [
            (InputPurpose::InputManifest, InputSourceKind::AuthenticatedRoot),
            (InputPurpose::TributeStream, InputSourceKind::AuthenticatedRoot),
        ]
    );
    assert_eq!(
        first.interval,
        UnitInterval::EntityIdRange(
            outbe_ocomp_protocol::unit::EntityIdHalfOpenRange {
                start: ids[0],
                end: Some(ids[256]),
            }
        )
    );

    lookups.clear();
    let second = planner
        .primary_unit_at(
            1,
            |ordinal| {
                lookups.push(ordinal);
                ids.get(ordinal as usize).copied()
            },
            &limits,
        )
        .unwrap();
    assert_eq!(lookups, [256]);
    assert_eq!(
        second.interval,
        UnitInterval::EntityIdRange(
            outbe_ocomp_protocol::unit::EntityIdHalfOpenRange {
                start: ids[256],
                end: None,
            }
        )
    );

    let plan = planner
        .commit_primary_catalog(ids.iter().copied(), &limits)
        .unwrap();
    let replay = planner
        .commit_primary_catalog(ids.iter().copied(), &limits)
        .unwrap();
    assert_eq!(plan, replay);
    assert_eq!(plan.primary_work_unit_count, 2);
    assert_eq!(plan.tribute_count, 257);
    assert_eq!(plan.wwd, 20_260_724);
    assert_eq!(plan.lysis_budget, U256::from(99_000_000_u64));
    assert_eq!(plan.logical_evaluation_time, 1_784_765_900);
    assert_eq!(
        plan.plan_hash(&limits).unwrap(),
        replay.plan_hash(&limits).unwrap()
    );

    let mut changed_budget = plan.clone();
    changed_budget.lysis_budget += U256::from(1);
    assert_ne!(
        changed_budget.plan_hash(&limits).unwrap(),
        plan.plan_hash(&limits).unwrap()
    );
}

#[test]
fn complete_lysis_dag_has_frozen_phase_counts_and_both_prefix_directions() {
    for primary_count in 1..=8 {
        let topology = LysisPlanTopologyV1::new(primary_count).unwrap();
        let padded = primary_count.next_power_of_two().max(2);
        let internal = padded - 1;
        let active_internal = (1..=topology.tree().height())
            .map(|level| primary_count.div_ceil(1_u32 << level))
            .sum::<u32>();

        assert_eq!(topology.phase_unit_count(UnitPhase::Enumerate), primary_count);
        assert_eq!(topology.phase_unit_count(UnitPhase::FidelityMap), primary_count);
        assert_eq!(topology.phase_unit_count(UnitPhase::FixedReduce), internal);
        assert_eq!(topology.phase_unit_count(UnitPhase::AmountMap), primary_count);
        assert_eq!(
            topology.phase_unit_count(UnitPhase::GratisPrefix),
            primary_count + active_internal
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::GratisPrefixDown),
            primary_count + active_internal
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::OutputFinalize),
            primary_count
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::OwnerShuffle),
            primary_count + active_internal
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::BucketShuffle),
            primary_count + active_internal
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::RootReduce),
            primary_count + internal
        );

        let prefix = topology
            .phase_position_at(UnitPhase::GratisPrefix, primary_count)
            .unwrap();
        assert_eq!(
            prefix,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 1,
                index: 0,
            }
        );
        let prefix_down = topology
            .phase_position_at(UnitPhase::GratisPrefixDown, 0)
            .unwrap();
        assert_eq!(
            prefix_down,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level: topology.tree().height(),
                index: 0,
            }
        );
        assert_eq!(
            topology
                .phase_position_at(
                    UnitPhase::GratisPrefixDown,
                    topology.phase_unit_count(UnitPhase::GratisPrefixDown) - 1,
                )
                .unwrap(),
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefixDown,
                level: 0,
                index: primary_count - 1,
            }
        );
    }
}

#[test]
fn complete_plan_cursor_uses_protocol_order_not_runtime_completion_order() {
    let topology = LysisPlanTopologyV1::new(2).unwrap();
    let positions = (0..topology.total_unit_count())
        .map(|ordinal| topology.plan_position_at(ordinal).unwrap())
        .collect::<Vec<_>>();
    let phases = positions
        .iter()
        .map(PlannedUnitPositionV1::phase)
        .collect::<Vec<_>>();
    let mut runs = Vec::new();
    for phase in phases {
        if runs.last() != Some(&phase) {
            runs.push(phase);
        }
    }
    assert_eq!(
        runs,
        [
            UnitPhase::Enumerate,
            UnitPhase::FidelityMap,
            UnitPhase::FixedReduce,
            UnitPhase::AmountMap,
            UnitPhase::GratisPrefix,
            UnitPhase::GratisPrefixDown,
            UnitPhase::OutputFinalize,
            UnitPhase::OwnerShuffle,
            UnitPhase::BucketShuffle,
            UnitPhase::RootReduce,
        ]
    );
}

#[test]
fn every_derived_producer_is_an_earlier_exact_plan_member() {
    for primary_count in 1..=8 {
        let topology = LysisPlanTopologyV1::new(primary_count).unwrap();
        let positions = (0..topology.total_unit_count())
            .map(|ordinal| topology.plan_position_at(ordinal).unwrap())
            .collect::<Vec<_>>();

        for (consumer_ordinal, consumer) in positions.iter().copied().enumerate() {
            let producers = topology.required_producers(consumer).unwrap();
            assert!(producers.len() <= 3);
            for producer in producers {
                let PlannedProducerV1::Unit(producer) = producer else {
                    continue;
                };
                let producer_ordinal = positions
                    .iter()
                    .position(|candidate| *candidate == producer)
                    .expect("producer is an exact member of the same plan");
                assert!(
                    producer_ordinal < consumer_ordinal,
                    "{producer:?} must precede {consumer:?}"
                );
                assert_ne!(producer, consumer, "a UnitId cannot depend on itself");
            }
        }
    }
}

#[test]
fn producer_membership_rejects_missing_duplicate_and_replaced_inputs() {
    let topology = LysisPlanTopologyV1::new(3).unwrap();
    let consumer = PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::FixedReduce,
        level: 1,
        index: 0,
    };
    let expected = topology.required_producers(consumer).unwrap();
    assert_eq!(expected.len(), 2);
    assert!(topology
        .validate_exact_producers(consumer, &expected)
        .is_ok());
    assert!(topology
        .validate_exact_producers(consumer, &expected[..1])
        .is_err());
    assert!(topology
        .validate_exact_producers(consumer, &[expected[0], expected[0]])
        .is_err());
    assert!(topology
        .validate_exact_producers(
            consumer,
            &[
                expected[0],
                PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::Enumerate,
                    ordinal: 2,
                }),
            ],
        )
        .is_err());
}

#[test]
fn amount_map_requires_the_matching_fidelity_leaf_not_only_the_global_fraction_table() {
    let topology = LysisPlanTopologyV1::new(3).unwrap();
    let consumer = PlannedUnitPositionV1::Primary {
        phase: UnitPhase::AmountMap,
        ordinal: 1,
    };
    let expected = topology.required_producers(consumer).unwrap();
    assert_eq!(
        expected,
        [
            PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                phase: UnitPhase::Enumerate,
                ordinal: 1,
            }),
            PlannedProducerV1::Unit(PlannedUnitPositionV1::Primary {
                phase: UnitPhase::FidelityMap,
                ordinal: 1,
            }),
            PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::FixedReduce,
                level: topology.tree().height(),
                index: 0,
            }),
        ]
    );
    assert!(topology
        .validate_exact_producers(consumer, &[expected[0], expected[2]])
        .is_err());
}

#[test]
fn fidelity_map_and_fixed_reduce_match_the_native_lysis_fraction_table() {
    let day = WorldwideDay::new(20_260_724);
    let mut tributes = (0..257_u32)
        .map(|ordinal| {
            let mut owner_bytes = [0_u8; 20];
            owner_bytes[16..].copy_from_slice(&(ordinal + 1).to_be_bytes());
            let owner = Address::from(owner_bytes);
            ObservedTributeV1 {
                tribute: TributeInputV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 978,
                    nominal_amount_minor: U256::from(1_000_000_u64) * SCALE_1E18,
                    reference_currency: 840,
                    tribute_price_minor: U256::ZERO,
                    exclude_from_intex_issuance: ordinal.is_multiple_of(11),
                },
                first_league: ObservationValueV1::Value(7),
                second_league: ObservationValueV1::Value(7),
                conditional_entry_price_minor: ObservationValueV1::Unavailable,
                nod_target_available: true,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|observed| observed.tribute.tribute_id);
    let total_nominal = tributes
        .iter()
        .map(|observed| observed.tribute.nominal_amount_minor)
        .sum::<U256>();
    let gratis_allocation = total_nominal * U256::from(32_u8) / U256::from(100_u8);
    let expected = execute(ProgramInputV1 {
        worldwide_day: day,
        logical_evaluation_time: 1_784_765_900,
        gratis_allocation,
        mandatory_entry_price_840: ObservationValueV1::Value(SCALE_1E18),
        tributes: tributes.clone(),
    })
    .unwrap();

    let first = fidelity_map(0, &tributes[..256]).unwrap();
    let second = fidelity_map(256, &tributes[256..]).unwrap();
    assert_eq!(first.observations.len(), 256);
    assert_eq!(second.observations.len(), 1);
    let aggregate = fidelity_reduce(&first.aggregate, &second.aggregate).unwrap();
    let actual = finalize_fi_fraction_table(&aggregate, gratis_allocation).unwrap();

    assert_eq!(aggregate.tribute_count, 257);
    assert_eq!(aggregate.checked_total_nominal, expected.total_nominal);
    assert_eq!(actual, expected.league_fractions);
}

#[test]
fn fidelity_phase_rejects_missing_mismatched_and_non_adjacent_evidence() {
    let day = WorldwideDay::new(20_260_724);
    let owner = Address::repeat_byte(9);
    let mut observed = ObservedTributeV1 {
        tribute: TributeInputV1 {
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            issuance_currency: 978,
            nominal_amount_minor: SCALE_1E18,
            reference_currency: 840,
            tribute_price_minor: U256::ZERO,
            exclude_from_intex_issuance: false,
        },
        first_league: ObservationValueV1::Unavailable,
        second_league: ObservationValueV1::Value(7),
        conditional_entry_price_minor: ObservationValueV1::Unavailable,
        nod_target_available: true,
    };
    assert!(fidelity_map(0, &[observed.clone()]).is_err());

    observed.first_league = ObservationValueV1::Value(6);
    assert!(fidelity_map(0, &[observed.clone()]).is_err());

    observed.first_league = ObservationValueV1::Value(7);
    let left = fidelity_map(0, &[observed.clone()]).unwrap();
    let right = fidelity_map(2, &[observed]).unwrap();
    assert!(fidelity_reduce(&left.aggregate, &right.aggregate).is_err());
}

#[test]
fn amount_and_output_finalize_phases_match_sequential_lysis_for_shard_cap_plus_one() {
    let day = WorldwideDay::new(20_260_724);
    let mut tributes = (0..257_u32)
        .map(|ordinal| {
            let mut owner_bytes = [0_u8; 20];
            owner_bytes[16..].copy_from_slice(&(ordinal + 1).to_be_bytes());
            let owner = Address::from(owner_bytes);
            ObservedTributeV1 {
                tribute: TributeInputV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 978,
                    nominal_amount_minor: U256::from(1_000_000_u64) * SCALE_1E18,
                    reference_currency: 840,
                    tribute_price_minor: U256::from(2_u8) * SCALE_1E18,
                    exclude_from_intex_issuance: ordinal.is_multiple_of(11),
                },
                first_league: ObservationValueV1::Value(7),
                second_league: ObservationValueV1::Value(7),
                conditional_entry_price_minor: ObservationValueV1::Unavailable,
                nod_target_available: true,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|observed| observed.tribute.tribute_id);
    let total_nominal = tributes
        .iter()
        .map(|observed| observed.tribute.nominal_amount_minor)
        .sum::<U256>();
    let lysis_budget = total_nominal * U256::from(32_u8) / U256::from(100_u8);
    let logical_time = 1_784_765_900;
    let entry_price = SCALE_1E18;
    let sequential = execute(ProgramInputV1 {
        worldwide_day: day,
        logical_evaluation_time: logical_time,
        gratis_allocation: lysis_budget,
        mandatory_entry_price_840: ObservationValueV1::Value(entry_price),
        tributes: tributes.clone(),
    })
    .unwrap();

    let fidelity_left = fidelity_map(0, &tributes[..256]).unwrap();
    let fidelity_right = fidelity_map(256, &tributes[256..]).unwrap();
    let fidelity_root =
        fidelity_reduce(&fidelity_left.aggregate, &fidelity_right.aggregate).unwrap();
    let fractions = finalize_fi_fraction_table(&fidelity_root, lysis_budget).unwrap();
    let amount_left = amount_map(
        0,
        &tributes[..256],
        &fidelity_left.observations,
        &fractions,
        entry_price,
    )
    .unwrap();
    let amount_right = amount_map(
        256,
        &tributes[256..],
        &fidelity_right.observations,
        &fractions,
        entry_price,
    )
    .unwrap();

    let left_summary = gratis_summary(
        0,
        &amount_left
            .ordered_records
            .iter()
            .map(|record| record.gratis_load_minor)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let right_summary = gratis_summary(
        256,
        &amount_right
            .ordered_records
            .iter()
            .map(|record| record.gratis_load_minor)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let prefixes = gratis_prefix_down(
        Some(lysis_budget),
        GratisSummaryValueV1::Summary(left_summary),
        GratisSummaryValueV1::Summary(right_summary),
    )
    .unwrap();
    let left_prefix = finalize_gratis_leaf(
        prefixes[0].as_ref().unwrap().incoming_remaining,
        0,
        &amount_left
            .ordered_records
            .iter()
            .map(|record| record.gratis_load_minor)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let right_prefix = finalize_gratis_leaf(
        prefixes[1].as_ref().unwrap().incoming_remaining,
        256,
        &amount_right
            .ordered_records
            .iter()
            .map(|record| record.gratis_load_minor)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let finalized_left = output_finalize(&amount_left, &left_prefix, logical_time).unwrap();
    let finalized_right = output_finalize(&amount_right, &right_prefix, logical_time).unwrap();
    let records = finalized_left
        .ordered_records
        .into_iter()
        .chain(finalized_right.ordered_records)
        .collect::<Vec<_>>();

    assert_eq!(
        records
            .iter()
            .map(|record| record.nod_action.clone())
            .collect::<Vec<_>>(),
        sequential.nod_actions
    );
    let mut contributors = records
        .into_iter()
        .filter_map(|record| record.contributor_action)
        .collect::<Vec<_>>();
    contributors.sort_by_key(|action| action.owner);
    assert_eq!(contributors, sequential.contributors);
    assert_eq!(right_prefix.outgoing_remaining, sequential.remaining_gratis);

    let mut wrong_fidelity_leaf = fidelity_left.observations;
    wrong_fidelity_leaf[0].tribute_id = tributes[256].tribute.tribute_id;
    assert!(amount_map(
        0,
        &tributes[..256],
        &wrong_fidelity_leaf,
        &fractions,
        entry_price,
    )
    .is_err());

    let mut wrong_prefix = left_prefix.clone();
    wrong_prefix.outgoing_remaining += U256::from(1);
    assert!(output_finalize(&amount_left, &wrong_prefix, logical_time).is_err());

    let mut wrong_amount_summary = amount_left;
    wrong_amount_summary.checked_segment_gratis_total += U256::from(1);
    assert!(output_finalize(&wrong_amount_summary, &left_prefix, logical_time).is_err());
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
                        nominal_amount_minor: SCALE_1E18,
                        reference_currency: 840,
                        tribute_price_minor: U256::ZERO,
                        exclude_from_intex_issuance: false,
                    },
                    first_league: ObservationValueV1::Value((ordinal % 3 + 1) as u16),
                    second_league: ObservationValueV1::Value((ordinal % 3 + 1) as u16),
                    conditional_entry_price_minor: ObservationValueV1::Unavailable,
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

        let FidelityReduceValueV1::Aggregate(root) =
            nodes.get(&(tree.height(), 0)).unwrap()
        else {
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
    assert_eq!(exact[0].as_ref().unwrap().incoming_remaining, Some(U256::from(257_u16)));
    assert_eq!(exact[1].as_ref().unwrap().incoming_remaining, Some(U256::from(1_u8)));
    let first_leaf =
        finalize_gratis_leaf(exact[0].as_ref().unwrap().incoming_remaining, 0, &first_loads)
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
        outbe_lysis::program_v1::ProgramErrorV1::GratisLoadExceedsRemaining {
            ordinal: 256
        }
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

fn entity_id(ordinal: u32) -> EntityId36 {
    let mut bytes = [0_u8; 36];
    bytes[32..].copy_from_slice(&ordinal.to_be_bytes());
    EntityId36(bytes)
}
