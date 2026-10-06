use super::*;

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
            let league = match ordinal % 3 {
                0 => 1,
                1 => 2048,
                _ => 4096,
            };
            ObservedTributeV1 {
                tribute: TributeInputV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 978,
                    nominal_amount_minor: U256::from(1_000_000_u64) * SIX_DECIMAL_SCALE,
                    reference_currency: 840,
                    tribute_price_minor: U256::ZERO,
                    exclude_from_intex_issuance: ordinal.is_multiple_of(11),
                },
                first_league: ObservationValueV1::Value(league),
                second_league: ObservationValueV1::Value(league),
                entry_price_minor: ObservationValueV1::Value(SIX_DECIMAL_SCALE),
                nod_target_available: true,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|observed| observed.tribute.tribute_id);
    let total_nominal = tributes
        .iter()
        .map(|observed| observed.tribute.nominal_amount_minor)
        .sum::<U256>();
    let lysis_limit_minor = total_nominal * U256::from(32_u8) / U256::from(100_u8);
    let expected = execute(ProgramInputV1 {
        worldwide_day: day,
        logical_evaluation_time: 1_784_765_900,
        lysis_limit_minor,
        tributes: tributes.clone(),
    })
    .unwrap();

    let first = fidelity_map(0, &tributes[..256]).unwrap();
    let second = fidelity_map(256, &tributes[256..]).unwrap();
    assert_eq!(first.observations.len(), 256);
    assert_eq!(second.observations.len(), 1);
    let aggregate = fidelity_reduce(&first.aggregate, &second.aggregate).unwrap();
    let actual = finalize_fi_fraction_table(&aggregate, lysis_limit_minor).unwrap();

    assert_eq!(aggregate.tribute_count, 257);
    assert_eq!(aggregate.checked_total_nominal, expected.total_nominal);
    assert_eq!(actual, expected.league_fractions);
    assert_eq!(
        actual.iter().map(|row| row.league).collect::<Vec<_>>(),
        [1, 2048, 4096]
    );
    assert!(actual
        .windows(2)
        .all(|pair| pair[0].fraction < pair[1].fraction));
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
            nominal_amount_minor: SIX_DECIMAL_SCALE,
            reference_currency: 840,
            tribute_price_minor: U256::ZERO,
            exclude_from_intex_issuance: false,
        },
        first_league: ObservationValueV1::Unavailable,
        second_league: ObservationValueV1::Value(7),
        entry_price_minor: ObservationValueV1::Value(SIX_DECIMAL_SCALE),
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
fn dust_cost_matches_between_worker_sequential_lysis_and_nod_settlement() {
    let day = WorldwideDay::new(20_260_918);
    let entry_price = U256::from(19);
    let mut tributes = [25_629_u64, 1_882, 2_529]
        .into_iter()
        .enumerate()
        .map(|(index, load)| {
            let mut item = observed(index as u32 + 1, day, load, 1, false);
            item.tribute.reference_currency = 840;
            item.entry_price_minor = ObservationValueV1::Value(entry_price);
            item
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|item| item.tribute.tribute_id);
    let budget = tributes
        .iter()
        .map(|item| item.tribute.nominal_amount_minor)
        .sum::<U256>();
    let logical_time = 1_789_689_600;
    let sequential = execute(ProgramInputV1 {
        worldwide_day: day,
        logical_evaluation_time: logical_time,
        lysis_limit_minor: budget,
        tributes: tributes.clone(),
    })
    .unwrap();

    let fidelity = fidelity_map(0, &tributes).unwrap();
    let fractions = finalize_fi_fraction_table(&fidelity.aggregate, budget).unwrap();
    let amount = amount_map(0, &tributes, &fidelity.observations, &fractions).unwrap();
    let limits = poc_schema_limits();
    let amount = decode_amount_run(&encode_amount_run(&amount, &limits).unwrap(), &limits).unwrap();
    let loads = amount
        .ordered_records
        .iter()
        .map(|record| record.gratis_load_minor)
        .collect::<Vec<_>>();
    let prefix = finalize_gratis_leaf(Some(budget), 0, &loads).unwrap();
    let finalized = output_finalize(&amount, &prefix).unwrap();
    let finalized = decode_finalized_output_run(
        &encode_finalized_output_run(&finalized, &limits).unwrap(),
        &limits,
    )
    .unwrap();
    let actions = finalized
        .ordered_records
        .iter()
        .map(|record| record.nod_action.clone())
        .collect::<Vec<_>>();
    assert_eq!(actions, sequential.nod_actions);
    assert_eq!(sequential.remaining_lysis_limit_minor, U256::ZERO);
    assert_eq!(prefix.outgoing_remaining, U256::ZERO);
    assert_eq!(amount.checked_segment_gratis_total, budget);
    for (action, tribute) in actions.iter().zip(&tributes) {
        assert_eq!(
            action.gratis_load_minor,
            tribute.tribute.nominal_amount_minor
        );
        assert_eq!(action.entry_price_minor, entry_price);
        assert_eq!(action.settlement_cost_minor, U256::from(1));
        assert_eq!(
            action.settlement_cost_minor,
            outbe_nod::api::settlement_cost_minor(entry_price, action.gratis_load_minor).unwrap()
        );
    }
}

#[test]
fn amount_and_output_finalize_phases_match_sequential_lysis_for_shard_cap_plus_one() {
    let day = WorldwideDay::new(20_260_724);
    let mut tributes = (0..257_u32)
        .map(|ordinal| {
            let mut owner_bytes = [0_u8; 20];
            owner_bytes[16..].copy_from_slice(&(ordinal + 1).to_be_bytes());
            let owner = Address::from(owner_bytes);
            let league = match ordinal % 3 {
                0 => 1,
                1 => 2048,
                _ => 4096,
            };
            ObservedTributeV1 {
                tribute: TributeInputV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_currency: 978,
                    nominal_amount_minor: U256::from(1_000_000_u64) * SIX_DECIMAL_SCALE,
                    reference_currency: 840,
                    tribute_price_minor: U256::from(2_u8) * SIX_DECIMAL_SCALE,
                    exclude_from_intex_issuance: ordinal.is_multiple_of(11),
                },
                first_league: ObservationValueV1::Value(league),
                second_league: ObservationValueV1::Value(league),
                entry_price_minor: ObservationValueV1::Value(SIX_DECIMAL_SCALE),
                nod_target_available: true,
            }
        })
        .collect::<Vec<_>>();
    tributes.sort_by_key(|observed| observed.tribute.tribute_id);
    let total_nominal = tributes
        .iter()
        .map(|observed| observed.tribute.nominal_amount_minor)
        .sum::<U256>();
    let lysis_limit_minor = total_nominal * U256::from(32_u8) / U256::from(100_u8);
    let logical_time = 1_784_765_900;
    let sequential = execute(ProgramInputV1 {
        worldwide_day: day,
        logical_evaluation_time: logical_time,
        lysis_limit_minor,
        tributes: tributes.clone(),
    })
    .unwrap();

    let fidelity_left = fidelity_map(0, &tributes[..256]).unwrap();
    let fidelity_right = fidelity_map(256, &tributes[256..]).unwrap();
    let fidelity_root =
        fidelity_reduce(&fidelity_left.aggregate, &fidelity_right.aggregate).unwrap();
    let fractions = finalize_fi_fraction_table(&fidelity_root, lysis_limit_minor).unwrap();
    assert_eq!(fractions, sequential.league_fractions);
    assert!(fractions
        .windows(2)
        .all(|pair| pair[0].fraction < pair[1].fraction));
    let loads = sequential
        .nod_actions
        .iter()
        .map(|action| (action.league_id, action.gratis_load_minor))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(loads[&1] < loads[&2048]);
    assert!(loads[&2048] < loads[&4096]);
    let amount_left =
        amount_map(0, &tributes[..256], &fidelity_left.observations, &fractions).unwrap();
    let amount_right = amount_map(
        256,
        &tributes[256..],
        &fidelity_right.observations,
        &fractions,
    )
    .unwrap();
    let limits = poc_schema_limits();
    let encoded_amount = encode_amount_run(&amount_right, &limits).unwrap();
    assert_eq!(
        decode_amount_run(&encoded_amount, &limits).unwrap(),
        amount_right
    );
    let mut trailing_amount = encoded_amount;
    trailing_amount.push(0);
    assert!(decode_amount_run(&trailing_amount, &limits).is_err());

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
        Some(lysis_limit_minor),
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
    let finalized_left = output_finalize(&amount_left, &left_prefix).unwrap();
    let finalized_right = output_finalize(&amount_right, &right_prefix).unwrap();
    assert_eq!(
        finalized_left.checked_tribute_nominal_total,
        amount_left
            .ordered_records
            .iter()
            .map(|record| record.nominal_amount_minor)
            .sum::<U256>()
    );
    assert_eq!(
        finalized_right.checked_tribute_nominal_total,
        amount_right
            .ordered_records
            .iter()
            .map(|record| record.nominal_amount_minor)
            .sum::<U256>()
    );
    assert_eq!(
        finalized_left
            .checked_tribute_nominal_total
            .checked_add(finalized_right.checked_tribute_nominal_total)
            .unwrap(),
        total_nominal
    );
    let encoded_finalized = encode_finalized_output_run(&finalized_right, &limits).unwrap();
    assert_eq!(
        decode_finalized_output_run(&encoded_finalized, &limits).unwrap(),
        finalized_right
    );
    let mut trailing_finalized = encoded_finalized;
    trailing_finalized.push(0);
    assert!(decode_finalized_output_run(&trailing_finalized, &limits).is_err());
    let records = finalized_left
        .ordered_records
        .iter()
        .chain(&finalized_right.ordered_records)
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(
        records
            .iter()
            .map(|record| record.nod_action.clone())
            .collect::<Vec<_>>(),
        sequential.nod_actions
    );
    let mut contributors = records
        .iter()
        .filter_map(|record| record.contributor_action.clone())
        .collect::<Vec<_>>();
    contributors.sort_by_key(|action| action.owner);
    assert_eq!(
        contributors
            .iter()
            .map(|action| (action.owner, action.nominal_amount_minor))
            .collect::<Vec<_>>(),
        sequential
            .contributors
            .iter()
            .map(|action| (action.owner, action.nominal_amount_minor))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        right_prefix.outgoing_remaining,
        sequential.remaining_lysis_limit_minor
    );

    let owner_left = shuffle_owners(&finalized_left).unwrap();
    let owner_right = shuffle_owners(&finalized_right).unwrap();
    let mut owner_records = owner_left
        .ordered_contributors
        .iter()
        .chain(&owner_right.ordered_contributors)
        .collect::<Vec<_>>();
    owner_records.sort_by_key(|action| (action.owner, action.source_tribute_id));
    assert_eq!(
        owner_records
            .iter()
            .map(|action| (action.owner, action.nominal_amount_minor))
            .collect::<Vec<_>>(),
        sequential
            .contributors
            .iter()
            .map(|action| (action.owner, action.nominal_amount_minor))
            .collect::<Vec<_>>()
    );

    let bucket_left = shuffle_buckets(&finalized_left).unwrap();
    let bucket_right = shuffle_buckets(&finalized_right).unwrap();
    assert!(owner_left.ordered_contributors.len() <= 256);
    assert!(bucket_left.ordered_records.len() <= 256);
    let mut bucket_ordinals = bucket_left
        .ordered_records
        .iter()
        .chain(&bucket_right.ordered_records)
        .map(|record| record.raw_ordinal)
        .collect::<Vec<_>>();
    bucket_ordinals.sort_unstable();
    assert_eq!(bucket_ordinals, (0..257_u32).collect::<Vec<_>>());

    let mut owner_chunk_sizes = Vec::new();
    let mut previous_merged_owner = None;
    let owner_summary = merge_owner_runs_streaming(
        CanonicalRunSpanV1 {
            start_run: 0,
            end_run: 1,
        },
        owner_left.ordered_contributors.clone(),
        CanonicalRunSpanV1 {
            start_run: 1,
            end_run: 2,
        },
        owner_right.ordered_contributors.clone(),
        64,
        |_, chunk| {
            owner_chunk_sizes.push(chunk.len());
            for action in chunk {
                assert!(previous_merged_owner.is_none_or(|owner| owner < action.owner));
                previous_merged_owner = Some(action.owner);
            }
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    assert_eq!(
        owner_summary.record_count as usize,
        sequential.contributors.len()
    );
    assert!(owner_chunk_sizes.iter().all(|count| *count <= 64));
    assert_eq!(owner_summary.chunk_count as usize, owner_chunk_sizes.len());

    let mut bucket_chunk_sizes = Vec::new();
    let mut previous_bucket_key = None;
    let bucket_summary = merge_bucket_runs_streaming(
        CanonicalRunSpanV1 {
            start_run: 0,
            end_run: 1,
        },
        bucket_left.ordered_records.clone(),
        CanonicalRunSpanV1 {
            start_run: 1,
            end_run: 2,
        },
        bucket_right.ordered_records.clone(),
        64,
        |_, chunk| {
            bucket_chunk_sizes.push(chunk.len());
            for record in chunk {
                let key = (record.bucket_key, record.raw_ordinal);
                assert!(previous_bucket_key.is_none_or(|previous| previous < key));
                previous_bucket_key = Some(key);
            }
            Ok::<_, ()>(())
        },
    )
    .unwrap();
    assert_eq!(bucket_summary.record_count, 257);
    assert!(bucket_chunk_sizes.iter().all(|count| *count <= 64));
    assert!(merge_bucket_runs_streaming(
        CanonicalRunSpanV1 {
            start_run: 1,
            end_run: 2,
        },
        bucket_right.ordered_records,
        CanonicalRunSpanV1 {
            start_run: 0,
            end_run: 1,
        },
        bucket_left.ordered_records,
        64,
        |_, _| Ok::<_, ()>(()),
    )
    .is_err());
    assert!(matches!(
        merge_owner_runs_streaming(
            CanonicalRunSpanV1 {
                start_run: 0,
                end_run: 1,
            },
            owner_left.ordered_contributors,
            CanonicalRunSpanV1 {
                start_run: 1,
                end_run: 2,
            },
            owner_right.ordered_contributors,
            64,
            |_, _| Err::<(), _>("persist failed"),
        ),
        Err(StreamingMergeErrorV1::Sink("persist failed"))
    ));

    let mut wrong_coverage = finalized_left.clone();
    wrong_coverage.ordered_records[0].raw_ordinal = 1;
    assert!(shuffle_owners(&wrong_coverage).is_err());
    assert!(shuffle_buckets(&wrong_coverage).is_err());

    let mut duplicate_owner = finalized_left.clone();
    let eligible_indices = duplicate_owner
        .ordered_records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record.contributor_action.as_ref().map(|_| index))
        .take(2)
        .collect::<Vec<_>>();
    let first_owner = duplicate_owner.ordered_records[eligible_indices[0]]
        .contributor_action
        .as_ref()
        .unwrap()
        .owner;
    duplicate_owner.ordered_records[eligible_indices[1]]
        .contributor_action
        .as_mut()
        .unwrap()
        .owner = first_owner;
    assert!(shuffle_owners(&duplicate_owner).is_err());

    let mut wrong_fidelity_leaf = fidelity_left.observations;
    wrong_fidelity_leaf[0].tribute_id = tributes[256].tribute.tribute_id;
    assert!(amount_map(0, &tributes[..256], &wrong_fidelity_leaf, &fractions,).is_err());

    let mut wrong_prefix = left_prefix.clone();
    wrong_prefix.outgoing_remaining += U256::from(1);
    assert!(output_finalize(&amount_left, &wrong_prefix).is_err());

    let mut wrong_amount_summary = amount_left;
    wrong_amount_summary.checked_segment_gratis_total += U256::from(1);
    assert!(output_finalize(&wrong_amount_summary, &left_prefix).is_err());
}

#[test]
fn output_finalize_commits_all_excluded_nominal_once_per_shard_and_checks_overflow() {
    let day = WorldwideDay::new(20_260_724);
    let make_record = |seed: u8| {
        let owner = Address::repeat_byte(seed);
        AmountRecordV1 {
            raw_ordinal: 0,
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            league_id: 7,
            nominal_amount_minor: U256::ZERO,
            gratis_fraction_fp: SIX_DECIMAL_SCALE,
            gratis_load_minor: U256::from(1),
            entry_price_minor: SIX_DECIMAL_SCALE,
            settlement_cost_minor: U256::from(1),
            issuance_currency: 840,
            reference_currency: 978,
            exclude_from_intex_issuance: true,
        }
    };
    let run = |nominals: [U256; 2]| {
        let mut ordered_records = vec![make_record(1), make_record(2)];
        ordered_records.sort_by_key(|record| record.tribute_id);
        for (ordinal, record) in ordered_records.iter_mut().enumerate() {
            record.raw_ordinal = u32::try_from(ordinal).unwrap();
            record.nominal_amount_minor = nominals[ordinal];
        }
        AmountRunV1 {
            start_ordinal: 0,
            end_ordinal: 2,
            ordered_records,
            checked_segment_gratis_total: U256::from(2),
        }
    };
    let prefix = GratisLeafPrefixV1 {
        segment_ordinal: 0,
        incoming_remaining: U256::from(10),
        outgoing_remaining: U256::from(8),
        first_error_ordinal: None,
    };

    let finalized = output_finalize(&run([U256::from(7), U256::from(11)]), &prefix).unwrap();
    assert_eq!(finalized.checked_tribute_nominal_total, U256::from(18));
    assert!(finalized
        .ordered_records
        .iter()
        .all(|record| record.contributor_action.is_none()));
    let limits = poc_schema_limits();
    let encoded = encode_finalized_output_run(&finalized, &limits).unwrap();
    assert_eq!(
        decode_finalized_output_run(&encoded, &limits).unwrap(),
        finalized
    );

    assert!(matches!(
        output_finalize(&run([U256::MAX, U256::from(1)]), &prefix),
        Err(ProgramErrorV1::TotalNominalOverflow { ordinal: 1 })
    ));
}
