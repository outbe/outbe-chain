use outbe_lysis::program_v1::planner::{
    primary_work_unit_count, LysisPlannerBindingsV1, LysisPlannerV1, PaddedBinaryTreeV1,
    PlannedUnitPositionV1, PlannerErrorV1, ReducerInputV1, LysisPlanTopologyV1,
    PRIMARY_WORK_SHARD_SIZE,
};
use outbe_ocomp_protocol::{
    common::EntityId36,
    local_control::poc_schema_limits,
    unit::{InputPurpose, InputSourceKind, UnitInterval, UnitPhase},
};
use alloy_primitives::B256;

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
    assert_eq!(plan.plan_hash(&limits).unwrap(), replay.plan_hash(&limits).unwrap());
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

fn entity_id(ordinal: u32) -> EntityId36 {
    let mut bytes = [0_u8; 36];
    bytes[32..].copy_from_slice(&ordinal.to_be_bytes());
    EntityId36(bytes)
}
