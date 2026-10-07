use super::*;

#[test]
fn gratis_scan_artifacts_are_typed_canonical_and_reject_trailing_bytes() {
    let limits = poc_schema_limits();
    let summary = GratisSegmentSummaryV1 {
        start_ordinal: 256,
        end_ordinal: 513,
        checked_segment_gratis_total: U256::from(77),
    };
    let encoded_summary = encode_gratis_segment_summary(&summary, &limits).unwrap();
    assert_eq!(
        decode_gratis_segment_summary(&encoded_summary, &limits).unwrap(),
        summary
    );

    let branch = GratisPrefixDownOutputV1::Branch([
        Some(GratisIncomingV1 {
            start_ordinal: 0,
            end_ordinal: 256,
            incoming_remaining: Some(U256::from(1_000)),
        }),
        Some(GratisIncomingV1 {
            start_ordinal: 256,
            end_ordinal: 513,
            incoming_remaining: Some(U256::from(700)),
        }),
    ]);
    let encoded_branch = encode_gratis_prefix_down_output(&branch, &limits).unwrap();
    assert_eq!(
        decode_gratis_prefix_down_output(&encoded_branch, &limits).unwrap(),
        branch
    );

    let leaf = GratisPrefixDownOutputV1::Leaf(GratisLeafPrefixV1 {
        segment_ordinal: 2,
        incoming_remaining: U256::from(700),
        outgoing_remaining: U256::from(623),
        first_error_ordinal: None,
    });
    let mut encoded_leaf = encode_gratis_prefix_down_output(&leaf, &limits).unwrap();
    assert_eq!(
        decode_gratis_prefix_down_output(&encoded_leaf, &limits).unwrap(),
        leaf
    );
    encoded_leaf.push(0);
    assert!(decode_gratis_prefix_down_output(&encoded_leaf, &limits).is_err());

    let interval = B256::repeat_byte(0x81);
    let left = (B256::repeat_byte(0x82), 256);
    let right = (B256::repeat_byte(0x83), 257);
    let complete = gratis_summary_coverage(interval, [Some(left), Some(right)]).unwrap();
    assert_eq!(complete.count, 513);
    assert_ne!(complete.root, B256::ZERO);
    assert_ne!(
        complete,
        gratis_summary_coverage(interval, [Some(left), None]).unwrap()
    );
    assert!(gratis_summary_coverage(interval, [None, Some(right)]).is_err());
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

        assert_eq!(
            topology.phase_unit_count(UnitPhase::Enumerate),
            primary_count
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::FidelityMap),
            primary_count
        );
        assert_eq!(topology.phase_unit_count(UnitPhase::FixedReduce), internal);
        assert_eq!(
            topology.phase_unit_count(UnitPhase::AmountMap),
            primary_count
        );
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
            primary_count * 2 - 1
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::BucketShuffle),
            primary_count * 2 - 1
        );
        assert_eq!(
            topology.phase_unit_count(UnitPhase::RootReduce),
            if primary_count == 1 {
                1
            } else {
                primary_count + internal
            }
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
fn exact_plan_positions_round_trip_to_their_global_ordinals() {
    for primary_count in 1..=8 {
        let topology = LysisPlanTopologyV1::new(primary_count).unwrap();
        for ordinal in 0..topology.total_unit_count() {
            let position = topology.plan_position_at(ordinal).unwrap();
            assert_eq!(topology.plan_ordinal_of(position).unwrap(), ordinal);
        }
    }

    let primary_count = primary_work_unit_count(1_000_000_000).unwrap();
    let topology = LysisPlanTopologyV1::new(primary_count).unwrap();
    let last = topology.total_unit_count() - 1;
    for ordinal in [0, primary_count - 1, primary_count, last / 2, last] {
        let position = topology.plan_position_at(ordinal).unwrap();
        assert_eq!(topology.plan_ordinal_of(position).unwrap(), ordinal);
    }
}

#[test]
fn three_shuffle_runs_have_only_real_binary_merges() {
    let topology = LysisPlanTopologyV1::new(3).unwrap();
    for phase in [UnitPhase::OwnerShuffle, UnitPhase::BucketShuffle] {
        assert_eq!(topology.phase_unit_count(phase), 5);

        let root = topology.phase_position_at(phase, 4).unwrap();
        assert_eq!(
            root,
            PlannedUnitPositionV1::RunSpan {
                phase,
                level: 2,
                index: 0,
                start_run: 0,
                end_run: 3,
            }
        );
        assert_eq!(
            topology.required_producers(root).unwrap(),
            [
                PlannedProducerV1::Unit(PlannedUnitPositionV1::RunSpan {
                    phase,
                    level: 1,
                    index: 0,
                    start_run: 0,
                    end_run: 2,
                }),
                PlannedProducerV1::Unit(PlannedUnitPositionV1::RunSpan {
                    phase,
                    level: 0,
                    index: 2,
                    start_run: 2,
                    end_run: 3,
                }),
            ]
        );
    }
}

#[test]
fn shuffle_topology_is_exact_for_small_boundaries_and_a_billion_tributes() {
    for primary_count in [1, 2, 3, 5, 256, 257] {
        let topology = LysisPlanTopologyV1::new(primary_count).unwrap();
        for phase in [UnitPhase::OwnerShuffle, UnitPhase::BucketShuffle] {
            let unit_count = primary_count * 2 - 1;
            assert_eq!(topology.phase_unit_count(phase), unit_count);

            let positions = (0..unit_count)
                .map(|ordinal| topology.phase_position_at(phase, ordinal).unwrap())
                .collect::<Vec<_>>();
            let root = *positions.last().unwrap();
            assert!(matches!(
                root,
                PlannedUnitPositionV1::RunSpan {
                    start_run: 0,
                    end_run,
                    ..
                } if end_run == primary_count
            ));

            for (consumer_ordinal, consumer) in positions
                .iter()
                .copied()
                .enumerate()
                .skip(primary_count as usize)
            {
                let producers = topology.required_producers(consumer).unwrap();
                assert_eq!(producers.len(), 2);
                for producer in producers {
                    let PlannedProducerV1::Unit(producer) = producer else {
                        panic!("shuffle topology must not contain CanonicalEmpty");
                    };
                    let producer_ordinal = positions
                        .iter()
                        .position(|candidate| *candidate == producer)
                        .expect("shuffle producer belongs to the same phase");
                    assert!(producer_ordinal < consumer_ordinal);
                }
            }
        }
    }

    let billion_primary = primary_work_unit_count(1_000_000_000).unwrap();
    assert_eq!(billion_primary, 3_906_250);
    let topology = LysisPlanTopologyV1::new(billion_primary).unwrap();
    let unit_count = topology.phase_unit_count(UnitPhase::OwnerShuffle);
    assert_eq!(unit_count, 7_812_499);
    assert!(matches!(
        topology
            .phase_position_at(UnitPhase::OwnerShuffle, unit_count - 1)
            .unwrap(),
        PlannedUnitPositionV1::RunSpan {
            start_run: 0,
            end_run: 3_906_250,
            ..
        }
    ));
}

#[test]
fn owner_merge_preserves_source_span_when_every_contributor_is_excluded() {
    let mut sink_calls = 0_u32;
    let summary = merge_owner_runs_streaming(
        CanonicalRunV1 {
            span: CanonicalRunSpanV1 {
                start_run: 0,
                end_run: 1,
            },
            records: Vec::new(),
        },
        CanonicalRunV1 {
            span: CanonicalRunSpanV1 {
                start_run: 1,
                end_run: 2,
            },
            records: Vec::new(),
        },
        256,
        |_, _| {
            sink_calls += 1;
            Ok::<_, ()>(())
        },
    )
    .unwrap();

    assert_eq!(
        summary.span,
        CanonicalRunSpanV1 {
            start_run: 0,
            end_run: 2,
        }
    );
    assert_eq!(summary.record_count, 0);
    assert_eq!(summary.chunk_count, 0);
    assert_eq!(summary.checked_eligible_nominal_total, U256::ZERO);
    assert_eq!(sink_calls, 0);
}

#[test]
fn prefix_down_reads_parent_prefix_and_immediate_child_summaries() {
    let topology = LysisPlanTopologyV1::new(3).unwrap();
    let root = PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefixDown,
        level: 2,
        index: 0,
    };
    assert_eq!(
        topology.required_producers(root).unwrap(),
        [
            PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 1,
                index: 0,
            }),
            PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 1,
                index: 1,
            }),
        ]
    );

    let internal = PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefixDown,
        level: 1,
        index: 1,
    };
    assert_eq!(
        topology.required_producers(internal).unwrap(),
        [
            PlannedProducerV1::Unit(root),
            PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 0,
                index: 2,
            }),
            PlannedProducerV1::CanonicalEmpty {
                purpose: InputPurpose::GratisPrefixTable,
                padded_ordinal: 3,
            },
        ]
    );

    let leaf = PlannedUnitPositionV1::TreeNode {
        phase: UnitPhase::GratisPrefixDown,
        level: 0,
        index: 2,
    };
    assert_eq!(
        topology.required_producers(leaf).unwrap(),
        [
            PlannedProducerV1::Unit(internal),
            PlannedProducerV1::Unit(PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::GratisPrefix,
                level: 0,
                index: 2,
            }),
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
