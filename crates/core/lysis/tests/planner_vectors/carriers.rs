use super::*;

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
fn enumerate_artifact_is_bounded_canonical_and_commits_raw_coverage() {
    let limits = poc_schema_limits();
    let day = WorldwideDay::new(20_260_724);
    let mut tributes = vec![tribute(1, day, 10, false), tribute(2, day, 20, true)];
    tributes.sort_by_key(|tribute| tribute.tribute_id);
    let run = enumerate_tributes(256, day, &tributes).unwrap();

    assert_eq!((run.start_ordinal, run.end_ordinal), (256, 258));
    assert_eq!(run.ordered_records[0].raw_ordinal, 256);
    assert_eq!(run.ordered_records[1].raw_ordinal, 257);
    assert_ne!(run.coverage_root().unwrap(), B256::ZERO);

    let encoded = encode_enumerated_run(&run, &limits).unwrap();
    assert_eq!(decode_enumerated_run(&encoded, &limits).unwrap(), run);

    let mut changed = encoded.clone();
    changed[24] ^= 1;
    assert!(decode_enumerated_run(&changed, &limits).is_err());

    let mut trailing = encoded;
    trailing.push(0);
    assert!(decode_enumerated_run(&trailing, &limits).is_err());
}

#[test]
fn enumerate_rejects_empty_oversized_and_noncanonical_shards() {
    let day = WorldwideDay::new(20_260_724);
    assert!(enumerate_tributes(0, day, &[]).is_err());

    let mut oversized = (0..257)
        .map(|index| tribute(index + 1, day, 1, false))
        .collect::<Vec<_>>();
    oversized.sort_by_key(|tribute| tribute.tribute_id);
    assert!(enumerate_tributes(0, day, &oversized).is_err());

    let duplicate = tribute(1, day, 1, false);
    assert!(enumerate_tributes(0, day, &[duplicate.clone(), duplicate]).is_err());
}

#[test]
fn fidelity_map_artifact_is_bounded_canonical_and_preserves_raw_coverage() {
    let limits = poc_schema_limits();
    let day = WorldwideDay::new(20_260_724);
    let mut observed = vec![
        observed(1, day, 10, 2, false),
        observed(2, day, 20, 3, true),
    ];
    observed.sort_by_key(|item| item.tribute.tribute_id);
    let output = fidelity_map(256, &observed).unwrap();

    let encoded = encode_fidelity_map_output(&output, &limits).unwrap();
    assert_eq!(
        decode_fidelity_map_output(&encoded, &limits).unwrap(),
        output
    );
    assert_eq!(output.coverage_root().unwrap(), {
        let enumerated = enumerate_tributes(
            256,
            day,
            &observed
                .iter()
                .map(|item| item.tribute.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        enumerated.coverage_root().unwrap()
    });

    let mut changed = encoded.clone();
    changed[24] ^= 1;
    assert!(decode_fidelity_map_output(&changed, &limits).is_err());

    let mut trailing = encoded;
    trailing.push(0);
    assert!(decode_fidelity_map_output(&trailing, &limits).is_err());
}

#[test]
fn constant_size_coverage_carriers_merge_to_the_canonical_full_raw_root() {
    let limits = poc_schema_limits();
    for total_count in [1_u32, 255, 256, 257, 513] {
        let records = (0..total_count)
            .map(|raw_ordinal| (raw_ordinal, WwdEntityId::from(entity_id(raw_ordinal))))
            .collect::<Vec<_>>();
        let primary_count = total_count.div_ceil(PRIMARY_WORK_SHARD_SIZE);
        let mut carriers = (0..primary_count)
            .map(|ordinal| {
                let start = ordinal * PRIMARY_WORK_SHARD_SIZE;
                let end = (start + PRIMARY_WORK_SHARD_SIZE).min(total_count);
                RawCoverageCarrierV1::from_records(
                    total_count,
                    &records[start as usize..end as usize],
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        if primary_count > 1 {
            let padded_count = primary_count.next_power_of_two();
            carriers.extend((primary_count..padded_count).map(|ordinal| {
                RawCoverageCarrierV1::canonical_empty(total_count, ordinal).unwrap()
            }));
            while carriers.len() > 1 {
                carriers = carriers
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| RawCoverageCarrierV1::merge(&pair[0], &pair[1]).unwrap())
                    .collect();
            }
        }
        let carrier = carriers.first().unwrap();
        let expected_records = records
            .iter()
            .map(|(ordinal, id)| {
                let mut encoded = [0_u8; 36];
                encoded[..4].copy_from_slice(&ordinal.to_be_bytes());
                encoded[4..].copy_from_slice(id.as_slice());
                encoded
            })
            .collect::<Vec<_>>();
        let expected_root = outbe_ocomp_protocol::ordered_list_root(
            outbe_ocomp_protocol::ListKind::RawTributeCoverage,
            &expected_records,
            outbe_ocomp_protocol::OrderedListLimits::new(
                expected_records.len(),
                40,
                expected_records.len().next_power_of_two() * 32,
            ),
        )
        .unwrap();
        assert_eq!(carrier.final_root(total_count).unwrap(), expected_root);

        let encoded = encode_raw_coverage_carrier(carrier, &limits).unwrap();
        assert_eq!(
            decode_raw_coverage_carrier(&encoded, &limits).unwrap(),
            *carrier
        );
        assert!(encoded.len() < 128);
    }
}

#[test]
fn coverage_carriers_reject_non_canonical_ranges_and_merge_order() {
    let total_count = 257_u32;
    let records = (0..total_count)
        .map(|raw_ordinal| (raw_ordinal, WwdEntityId::from(entity_id(raw_ordinal))))
        .collect::<Vec<_>>();
    let left = RawCoverageCarrierV1::from_records(total_count, &records[..256]).unwrap();
    let right = RawCoverageCarrierV1::from_records(total_count, &records[256..]).unwrap();

    assert!(RawCoverageCarrierV1::merge(&right, &left).is_err());

    let mut non_contiguous = records[..256].to_vec();
    non_contiguous[127].0 += 1;
    assert!(RawCoverageCarrierV1::from_records(total_count, &non_contiguous).is_err());

    let limits = poc_schema_limits();
    let mut trailing = encode_raw_coverage_carrier(&left, &limits).unwrap();
    trailing.push(0);
    assert!(decode_raw_coverage_carrier(&trailing, &limits).is_err());
}

#[test]
fn fixed_reduce_output_is_canonical_and_binds_aggregate_carrier_and_fractions() {
    let limits = poc_schema_limits();
    let records = (0..257_u32)
        .map(|raw_ordinal| (raw_ordinal, WwdEntityId::from(entity_id(raw_ordinal))))
        .collect::<Vec<_>>();
    let left = RawCoverageCarrierV1::from_records(257, &records[..256]).unwrap();
    let right = RawCoverageCarrierV1::from_records(257, &records[256..]).unwrap();
    let coverage = RawCoverageCarrierV1::merge(&left, &right).unwrap();
    let output = FixedReduceOutputV1 {
        aggregate: Some(FidelityAggregateV1 {
            start_ordinal: 0,
            end_ordinal: 257,
            tribute_count: 257,
            checked_total_nominal: U256::from(257),
            ordered_league_partials: vec![FidelityLeaguePartialV1 {
                league_id: 7,
                count: 257,
                nominal_amount_minor: U256::from(257),
            }],
        }),
        coverage,
        ordered_fractions: vec![LeagueFractionV1 {
            league: 7,
            fraction: U256::from(9),
        }],
    };

    let encoded = encode_fixed_reduce_output(&output, &limits).unwrap();
    assert_eq!(
        decode_fixed_reduce_output(&encoded, &limits).unwrap(),
        output
    );
    let mut trailing = encoded;
    trailing.push(0);
    assert!(decode_fixed_reduce_output(&trailing, &limits).is_err());

    let mut mismatched = output;
    mismatched.coverage.end_ordinal = 256;
    assert!(encode_fixed_reduce_output(&mismatched, &limits).is_err());
}

#[test]
fn root_reduce_summary_is_bounded_canonical_and_rejects_cross_list_substitution() {
    let limits = poc_schema_limits();
    let carrier = |list_kind, real_count, subtree_height, marker| LysisListSubtreeCarrierV1 {
        list_kind,
        start_ordinal: 0,
        real_count,
        subtree_height,
        subtree_index: 0,
        tree_root: B256::repeat_byte(marker),
    };
    let summary = RootReduceSummaryV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 3,
        plan_hash: B256::repeat_byte(4),
        covered_primary_start: 0,
        covered_primary_count: 2,
        nod_actions: carrier(outbe_ocomp_protocol::ListKind::NodActions, 257, 9, 11),
        bucket_records: carrier(outbe_ocomp_protocol::ListKind::BucketRecords, 257, 9, 12),
        contributor_actions: carrier(outbe_ocomp_protocol::ListKind::ContributorActions, 1, 9, 13),
        output_manifest_entries: carrier(
            outbe_ocomp_protocol::ListKind::CompleteOutputManifest,
            2,
            1,
            14,
        ),
        result_chunk_hashes: carrier(outbe_ocomp_protocol::ListKind::ResultChunkHashes, 2, 1, 15),
        tribute_count: 257,
        nod_count: 257,
        bucket_count: 257,
        contributor_count: 1,
        tribute_nominal_total: U256::from(10_000),
        eligible_nominal_total: U256::from(100),
        lysis_allocation_minor: U256::from(90),
        nod_cost_total: U256::from(9_000),
        first_error_ordinal: None,
    };

    let encoded = encode_root_reduce_summary(&summary, &limits).unwrap();
    assert!(encoded.len() < 512);
    assert_eq!(
        decode_root_reduce_summary(&encoded, &limits).unwrap(),
        summary
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert!(decode_root_reduce_summary(&trailing, &limits).is_err());

    let mut substituted = summary.clone();
    substituted.nod_actions.list_kind = outbe_ocomp_protocol::ListKind::BucketRecords;
    assert!(encode_root_reduce_summary(&substituted, &limits).is_err());

    let mut wrong_span = summary.clone();
    wrong_span.contributor_actions.subtree_height = 8;
    assert!(encode_root_reduce_summary(&wrong_span, &limits).is_err());

    let mut manifest_per_nod = summary.clone();
    manifest_per_nod.output_manifest_entries.real_count = manifest_per_nod.nod_count;
    manifest_per_nod.output_manifest_entries.subtree_height = 9;
    assert!(encode_root_reduce_summary(&manifest_per_nod, &limits).is_err());

    let mut inconsistent = summary;
    inconsistent.contributor_count = 2;
    assert!(encode_root_reduce_summary(&inconsistent, &limits).is_err());
}

#[test]
fn root_reduce_output_is_a_closed_leaf_or_node_payload() {
    let limits = poc_schema_limits();
    let entry = OutputManifestEntryV1 {
        chunk_ordinal: 0,
        result_chunk_hash: B256::repeat_byte(0x41),
        result_chunk_ref: CasObjectRefV1 {
            transport_digest: B256::repeat_byte(0x42),
            encoded_bytes: 128,
            expected_ocb1_kind: Some(ObjectKind::ResultChunkV1.tag()),
        },
    };
    let carrier = |kind, item: &[u8]| {
        LysisListSubtreeCarrierV1::from_primary_page(kind, 0, &[item], 512).unwrap()
    };
    let summary = RootReduceSummaryV1 {
        protocol_bundle_hash: B256::repeat_byte(1),
        job_id: B256::repeat_byte(2),
        attempt: 3,
        plan_hash: B256::repeat_byte(4),
        covered_primary_start: 0,
        covered_primary_count: 1,
        nod_actions: carrier(ListKind::NodActions, &[1]),
        bucket_records: carrier(ListKind::BucketRecords, &[2]),
        contributor_actions: carrier(ListKind::ContributorActions, &[3]),
        output_manifest_entries: carrier(
            ListKind::CompleteOutputManifest,
            &entry.encode_canonical_record(&limits).unwrap(),
        ),
        result_chunk_hashes: carrier(
            ListKind::ResultChunkHashes,
            entry.result_chunk_hash.as_slice(),
        ),
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 1,
        contributor_count: 1,
        tribute_nominal_total: U256::from(100),
        eligible_nominal_total: U256::from(100),
        lysis_allocation_minor: U256::from(10),
        nod_cost_total: U256::from(90),
        first_error_ordinal: None,
    };

    let mut node_summary = summary.clone();
    for carrier in [
        &mut node_summary.nod_actions,
        &mut node_summary.bucket_records,
        &mut node_summary.contributor_actions,
    ] {
        carrier.subtree_height = 9;
    }
    node_summary.output_manifest_entries.subtree_height = 1;
    node_summary.result_chunk_hashes.subtree_height = 1;

    for output in [
        RootReduceOutputV1::Leaf {
            summary: summary.clone(),
            output_manifest_entry: entry.clone(),
        },
        RootReduceOutputV1::Node {
            summary: node_summary,
        },
    ] {
        let encoded = encode_root_reduce_output(&output, &limits).unwrap();
        assert_eq!(
            decode_root_reduce_output(&encoded, &limits).unwrap(),
            output
        );

        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode_root_reduce_output(&trailing, &limits).is_err());
    }

    let mut substituted_entry = entry;
    substituted_entry.result_chunk_hash = B256::repeat_byte(0xff);
    assert!(encode_root_reduce_output(
        &RootReduceOutputV1::Leaf {
            summary: summary.clone(),
            output_manifest_entry: substituted_entry,
        },
        &limits,
    )
    .is_err());
    assert!(encode_root_reduce_output(&RootReduceOutputV1::Node { summary }, &limits,).is_err());
}

#[test]
fn fixed_capacity_result_carriers_merge_by_position_without_becoming_dense_roots() {
    let first_items = (0_u16..256)
        .map(|value| value.to_be_bytes().to_vec())
        .collect::<Vec<_>>();
    let second_items = vec![b"last".to_vec()];
    let first = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::ContributorActions,
        0,
        &first_items,
        16,
    )
    .unwrap();
    let second = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::ContributorActions,
        1,
        &second_items,
        16,
    )
    .unwrap();
    let merged = first.merge_adjacent(second).unwrap();

    assert_eq!(first.subtree_height, 8);
    assert_eq!(second.start_ordinal, PRIMARY_WORK_SHARD_SIZE);
    assert_eq!(merged.real_count, 257);
    assert_eq!(merged.subtree_height, 9);
    assert_eq!(merged.subtree_index, 0);

    let mut dense_items = first_items;
    dense_items.extend(second_items);
    let dense_root = ordered_list_root(
        ListKind::ContributorActions,
        &dense_items,
        OrderedListLimits::new(512, 16, 512 * 32),
    )
    .unwrap();
    let merged_dense_root = outbe_ocomp_protocol::list::root_hash(
        ListKind::ContributorActions,
        257,
        merged.subtree_height,
        merged.tree_root,
    )
    .unwrap();
    assert_eq!(merged_dense_root, dense_root);

    let sparse_items = vec![b"only".to_vec()];
    let sparse = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::ContributorActions,
        0,
        &sparse_items,
        16,
    )
    .unwrap();
    let empty =
        LysisListSubtreeCarrierV1::canonical_empty_primary_page(ListKind::ContributorActions, 1)
            .unwrap();
    assert!(!empty.tree_root.is_zero());
    assert_eq!(
        empty,
        LysisListSubtreeCarrierV1::canonical_empty_primary_page(ListKind::ContributorActions, 1,)
            .unwrap()
    );

    let sparse_coverage = sparse.merge_adjacent(empty).unwrap();
    let sparse_wrapped = outbe_ocomp_protocol::list::root_hash(
        ListKind::ContributorActions,
        1,
        sparse_coverage.subtree_height,
        sparse_coverage.tree_root,
    )
    .unwrap();
    let canonical_sparse = ordered_list_root(
        ListKind::ContributorActions,
        &sparse_items,
        OrderedListLimits::new(1, 16, 32),
    )
    .unwrap();
    assert_ne!(sparse_wrapped, canonical_sparse);

    let non_adjacent =
        LysisListSubtreeCarrierV1::canonical_empty_primary_page(ListKind::ContributorActions, 2)
            .unwrap();
    assert!(sparse.merge_adjacent(non_adjacent).is_err());
    let wrong_kind =
        LysisListSubtreeCarrierV1::canonical_empty_primary_page(ListKind::NodActions, 1).unwrap();
    assert!(sparse.merge_adjacent(wrong_kind).is_err());
}

#[test]
fn root_reduce_summaries_merge_only_adjacent_complete_prefixes() {
    let summary = |primary_ordinal: u32,
                   tribute_count: u32,
                   contributor_count: u32,
                   marker: u8,
                   first_error_ordinal: Option<u32>| {
        let actions = vec![vec![marker]; usize::try_from(tribute_count).unwrap()];
        let contributors = vec![vec![marker]; usize::try_from(contributor_count).unwrap()];
        let manifest_entry = vec![vec![marker]];
        let result_hash = vec![vec![marker; 32]];
        RootReduceSummaryV1 {
            protocol_bundle_hash: B256::repeat_byte(1),
            job_id: B256::repeat_byte(2),
            attempt: 3,
            plan_hash: B256::repeat_byte(4),
            covered_primary_start: primary_ordinal,
            covered_primary_count: 1,
            nod_actions: LysisListSubtreeCarrierV1::from_primary_page(
                ListKind::NodActions,
                primary_ordinal,
                &actions,
                16,
            )
            .unwrap(),
            bucket_records: LysisListSubtreeCarrierV1::from_primary_page(
                ListKind::BucketRecords,
                primary_ordinal,
                &actions,
                16,
            )
            .unwrap(),
            contributor_actions: LysisListSubtreeCarrierV1::from_primary_page(
                ListKind::ContributorActions,
                primary_ordinal,
                &contributors,
                16,
            )
            .unwrap(),
            output_manifest_entries: LysisListSubtreeCarrierV1::from_primary_page(
                ListKind::CompleteOutputManifest,
                primary_ordinal,
                &manifest_entry,
                16,
            )
            .unwrap(),
            result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
                ListKind::ResultChunkHashes,
                primary_ordinal,
                &result_hash,
                32,
            )
            .unwrap(),
            tribute_count,
            nod_count: tribute_count,
            bucket_count: tribute_count,
            contributor_count,
            tribute_nominal_total: U256::from(tribute_count) * U256::from(10),
            eligible_nominal_total: U256::from(tribute_count) * U256::from(8),
            lysis_allocation_minor: U256::from(tribute_count) * U256::from(3),
            nod_cost_total: U256::from(tribute_count) * U256::from(7),
            first_error_ordinal,
        }
    };

    let left = summary(0, 256, 1, 11, Some(1));
    let right = summary(1, 1, 1, 12, Some(256));
    let merged = left.clone().merge_adjacent(right.clone()).unwrap();
    assert_eq!(merged.covered_primary_start, 0);
    assert_eq!(merged.covered_primary_count, 2);
    assert_eq!(merged.tribute_count, 257);
    assert_eq!(merged.contributor_count, 2);
    assert_eq!(merged.tribute_nominal_total, U256::from(2_570));
    assert_eq!(merged.first_error_ordinal, Some(1));
    assert_eq!(merged.nod_actions.subtree_height, 9);
    assert_eq!(merged.output_manifest_entries.subtree_height, 1);
    assert_eq!(merged.result_chunk_hashes.subtree_height, 1);

    assert!(right.clone().merge_adjacent(left.clone()).is_err());
    let mut mismatched = right.clone();
    mismatched.plan_hash = B256::repeat_byte(99);
    assert!(left.clone().merge_adjacent(mismatched).is_err());
    let mut overflowing = left.clone();
    overflowing.tribute_nominal_total = U256::MAX;
    assert!(overflowing.merge_adjacent(right.clone()).is_err());

    // Contributor pages are globally owner-sorted, while Tribute totals are
    // partitioned by the primary Tribute shard. A valid contributor page can
    // therefore carry more nominal value than the unrelated Tribute page at
    // the same ordinal. Conservation only becomes meaningful at the complete
    // root, after all pages are reduced.
    let mut owner_paged_left = left.clone();
    owner_paged_left.tribute_nominal_total = U256::from(1_000);
    owner_paged_left.eligible_nominal_total = U256::from(2_000);
    let mut owner_paged_right = right.clone();
    owner_paged_right.tribute_nominal_total = U256::from(1_570);
    owner_paged_right.eligible_nominal_total = U256::ZERO;
    encode_root_reduce_summary(&owner_paged_left, &poc_schema_limits())
        .expect("globally owner-paged leaf is a valid reducer summary");
    let globally_conserved = owner_paged_left
        .merge_adjacent(owner_paged_right)
        .expect("adjacent owner pages reduce independently of primary Tribute totals");
    assert_eq!(globally_conserved.eligible_nominal_total, U256::from(2_000));
    assert_eq!(globally_conserved.tribute_nominal_total, U256::from(2_570));

    let empty_summary = |primary_ordinal| RootReduceSummaryV1 {
        protocol_bundle_hash: left.protocol_bundle_hash,
        job_id: left.job_id,
        attempt: left.attempt,
        plan_hash: left.plan_hash,
        covered_primary_start: primary_ordinal,
        covered_primary_count: 0,
        nod_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::NodActions,
            primary_ordinal,
        )
        .unwrap(),
        bucket_records: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::BucketRecords,
            primary_ordinal,
        )
        .unwrap(),
        contributor_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ContributorActions,
            primary_ordinal,
        )
        .unwrap(),
        output_manifest_entries: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::CompleteOutputManifest,
            primary_ordinal,
        )
        .unwrap(),
        result_chunk_hashes: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ResultChunkHashes,
            primary_ordinal,
        )
        .unwrap(),
        tribute_count: 0,
        nod_count: 0,
        bucket_count: 0,
        contributor_count: 0,
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        nod_cost_total: U256::ZERO,
        first_error_ordinal: None,
    };
    let padded = left.clone().merge_adjacent(empty_summary(1)).unwrap();
    assert_eq!(padded.covered_primary_count, 1);
    assert_eq!(padded.result_chunk_hashes.subtree_height, 1);
    assert!(empty_summary(0).merge_adjacent(right).is_err());
}
