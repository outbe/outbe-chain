use super::*;

fn checkpoint() -> OracleCheckpointV1 {
    OracleCheckpointV1 {
        height: 20,
        block_hash: alloy_primitives::B256::repeat_byte(1),
        state_root: alloy_primitives::B256::repeat_byte(2),
    }
}

fn owned(indices: &[usize]) -> Vec<OracleMemberV1> {
    indices
        .iter()
        .map(|&index| OracleMemberV1 {
            index,
            address: Address::repeat_byte(u8::try_from(index + 1).unwrap()),
            port: 8000 + u16::try_from(index).unwrap(),
            node_pid: 100 + u32::try_from(index).unwrap(),
        })
        .collect()
}

fn reads() -> Vec<PublicationRead> {
    [8000, 8001, 8002, 8003, 8004]
        .into_iter()
        .map(|port| PublicationRead {
            port,
            finalized: 22,
            checkpoint: checkpoint(),
            rate: EXPECTED_RATE,
            volume: Some(U256::from(100)),
            oracle_block: 19,
            oracle_timestamp: 100,
            finalized_timestamp: 110,
        })
        .collect()
}

#[test]
fn cohort_uses_active_identity_not_bootstrap_size_or_dense_indices() {
    let all = owned(&[0, 1, 2, 3, 4]);
    let active = all.iter().map(|member| member.address).collect::<Vec<_>>();
    let four = plan_cohort(checkpoint(), &active[..4], &all, false).unwrap();
    assert_eq!(four.feeder_indices, [0, 1, 2]);
    assert_eq!(four.members.len(), 4); // owned but non-ACTIVE joiner excluded
    let five = plan_cohort(checkpoint(), &active, &all, false).unwrap();
    assert_eq!(five.feeder_indices, [0, 1, 2, 3]);
    assert_eq!(five.members.len(), 5);
    assert_eq!(five.quorum, 4);
    let sparse = owned(&[0, 2, 4, 7, 9]);
    let active = sparse
        .iter()
        .map(|member| member.address)
        .collect::<Vec<_>>();
    assert_eq!(
        plan_cohort(checkpoint(), &active, &sparse, false)
            .unwrap()
            .feeder_indices,
        [0, 2, 4, 7]
    );
}

#[test]
fn rebuilt_cohort_binds_new_donor_pid_and_requires_post_restart_publication() {
    let mut owners = owned(&[0, 1, 2, 3]);
    let active = owners
        .iter()
        .map(|member| member.address)
        .collect::<Vec<_>>();
    let old = plan_cohort(checkpoint(), &active, &owners, false).unwrap();
    let old_pid = old.members[3].node_pid;
    owners[3].node_pid = old_pid + 100;
    let new_checkpoint = OracleCheckpointV1 {
        height: 21,
        block_hash: alloy_primitives::B256::repeat_byte(3),
        state_root: alloy_primitives::B256::repeat_byte(4),
    };
    let rebuilt = plan_cohort(new_checkpoint, &active, &owners, false).unwrap();
    assert_eq!(old.members[3].node_pid, old_pid);
    assert_eq!(rebuilt.members[3].node_pid, owners[3].node_pid);
    assert_ne!(rebuilt.members[3].node_pid, old.members[3].node_pid);
    assert_eq!(rebuilt.members, owners);
    assert_eq!(rebuilt.quorum, old.quorum);
    assert_eq!(rebuilt.feeder_indices, old.feeder_indices);
    validate_membership(&rebuilt, &active).unwrap();
    let lower_bound = publication_lower_bound(Some(19), rebuilt.checkpoint.height);
    assert_eq!(lower_bound, 21);
    let mut observations = reads();
    observations.truncate(4);
    let ports = rebuilt
        .members
        .iter()
        .map(|member| member.port)
        .collect::<Vec<_>>();
    for observation in &mut observations {
        observation.finalized = 22;
        observation.checkpoint = OracleCheckpointV1 {
            height: 22,
            block_hash: alloy_primitives::B256::repeat_byte(5),
            state_root: alloy_primitives::B256::repeat_byte(6),
        };
        observation.oracle_block = lower_bound;
    }
    assert!(!evaluate_publication(&ports, &observations, lower_bound, EXPECTED_RATE).unwrap());
    for observation in &mut observations {
        observation.oracle_block = lower_bound + 1;
    }
    assert!(evaluate_publication(&ports, &observations, lower_bound, EXPECTED_RATE).unwrap());
}

#[test]
fn admission_handoff_waits_for_five_finalized_identities_without_changing_generic_planning() {
    let all = owned(&[0, 1, 2, 3, 4]);
    let expected = all.iter().map(|member| member.address).collect::<Vec<_>>();
    // Primary latest may already report five, but the common finalized
    // checkpoint still has only four. It cannot authorize admission launch.
    let before = plan_cohort(checkpoint(), &expected[..4], &all, false).unwrap();
    assert!(validate_membership(&before, &expected).is_err());
    // Generic callers still legitimately exclude an owned PENDING joiner.
    assert_eq!(before.members.len(), 4);
    assert_eq!(before.feeder_indices, [0, 1, 2]);

    let admitted_checkpoint = OracleCheckpointV1 {
        height: 21,
        block_hash: alloy_primitives::B256::repeat_byte(3),
        state_root: alloy_primitives::B256::repeat_byte(4),
    };
    let admitted = plan_cohort(admitted_checkpoint.clone(), &expected, &all, false).unwrap();
    validate_membership(&admitted, &expected).unwrap();
    assert_eq!(admitted.checkpoint, admitted_checkpoint);
    assert_eq!(admitted.members, all);
    assert_eq!(admitted.quorum, 4);
    assert_eq!(admitted.feeder_indices, [0, 1, 2, 3]);
    let mut wrong_identity = expected;
    wrong_identity[4] = Address::repeat_byte(99);
    assert!(validate_membership(&admitted, &wrong_identity).is_err());
}

#[test]
fn cohort_rejects_unmapped_duplicate_empty_and_changed_membership() {
    let all = owned(&[0, 1, 2, 3, 4]);
    let active = all.iter().map(|member| member.address).collect::<Vec<_>>();
    assert!(plan_cohort(checkpoint(), &[], &all, false).is_err());
    assert!(plan_cohort(checkpoint(), &active, &all[..4], false).is_err());
    assert!(plan_cohort(checkpoint(), &[active[0], active[0]], &all, false).is_err());
    for field in 0..4 {
        let mut wrong = all.clone();
        match field {
            0 => wrong[4].address = wrong[0].address,
            1 => wrong[4].index = wrong[0].index,
            2 => wrong[4].port = wrong[0].port,
            _ => wrong[4].node_pid = wrong[0].node_pid,
        }
        assert!(plan_cohort(checkpoint(), &active, &wrong, false).is_err());
    }
    let cohort = plan_cohort(checkpoint(), &active, &all, false).unwrap();
    assert!(validate_membership(&cohort, &active).is_ok());
    assert!(validate_membership(&cohort, &active[..4]).is_err());
    let mut wrong = active.clone();
    wrong[4] = Address::repeat_byte(99);
    assert!(validate_membership(&cohort, &wrong).is_err());
}

#[test]
fn overlapping_negative_fixture_keeps_four_explicit_feeders() {
    let all = owned(&[0, 1, 2, 3, 4]);
    let active = all.iter().map(|member| member.address).collect::<Vec<_>>();
    let cohort = plan_cohort(checkpoint(), &active[..4], &all, true).unwrap();
    assert_eq!(cohort.feeder_indices, [0, 1, 2, 3]);
    assert_eq!(cohort.quorum, 3);
    assert!(plan_cohort(checkpoint(), &active, &all, true).is_err());
}

#[test]
fn publication_requires_all_five_pinned_finalized_peers() {
    let good = reads();
    let ports = good.iter().map(|read| read.port).collect::<Vec<_>>();
    assert!(evaluate_publication(&ports, &good, 18, EXPECTED_RATE).unwrap());
    assert!(evaluate_publication(&ports, &good[..4], 18, EXPECTED_RATE).is_err());
    for field in 0..8 {
        let mut bad = good.clone();
        match field {
            0 => bad[4].port = 8000,
            1 => bad[4].finalized = 19,
            2 => bad[4].checkpoint.block_hash = alloy_primitives::B256::ZERO,
            3 => bad[4].checkpoint.state_root = alloy_primitives::B256::ZERO,
            4 => bad[4].rate += U256::ONE,
            5 => bad[4].volume = None,
            6 => bad[4].oracle_block = 18,
            _ => bad[4].finalized_timestamp += 1,
        }
        assert!(
            evaluate_publication(&ports, &bad, 18, EXPECTED_RATE).is_err(),
            "field {field}"
        );
    }
}

#[test]
fn publication_advance_ttl_and_future_time_are_not_masked() {
    let good = reads();
    let ports = good.iter().map(|read| read.port).collect::<Vec<_>>();
    assert!(!evaluate_publication(&ports, &good, 19, EXPECTED_RATE).unwrap());
    assert!(!evaluate_publication(&ports, &good, 18, U256::ONE).unwrap());
    let mut empty = good.clone();
    for read in &mut empty {
        read.volume = None;
    }
    assert!(!evaluate_publication(&ports, &empty, 18, EXPECTED_RATE).unwrap());
    for (age, expected) in [(FX_TTL_SECS, true), (FX_TTL_SECS + 1, false)] {
        let mut changed = good.clone();
        for read in &mut changed {
            read.finalized_timestamp = read.oracle_timestamp + age;
        }
        assert_eq!(
            evaluate_publication(&ports, &changed, 18, EXPECTED_RATE).unwrap(),
            expected
        );
    }
    let mut future = good;
    for read in &mut future {
        read.oracle_timestamp = read.finalized_timestamp + 1;
    }
    assert!(evaluate_publication(&ports, &future, 18, EXPECTED_RATE).is_err());
}

#[test]
fn restart_barrier_excludes_unrecorded_pre_restart_publication() {
    assert_eq!(publication_lower_bound(Some(10), 18), 18);
    assert_eq!(publication_lower_bound(Some(22), 18), 22);
    let mut old = reads();
    for read in &mut old {
        read.oracle_block = 18;
    }
    let ports = old.iter().map(|read| read.port).collect::<Vec<_>>();
    assert!(!evaluate_publication(
        &ports,
        &old,
        publication_lower_bound(Some(10), 18),
        EXPECTED_RATE
    )
    .unwrap());
}

#[test]
fn controlled_quote_uses_the_canonical_six_decimal_scale() {
    assert_eq!(MOCK_PRICE, "1.000000");
    assert_eq!(EXPECTED_RATE, U256::from(1_000_000));
    assert_eq!(FX_TTL_SECS, 6 * 60 * 60);
    assert_eq!(scale6_quote(U256::from(1_080_001_u64)), "1.080001");
    assert_eq!(scale6_quote(U256::from(2_u64)), "0.000002");
}

#[test]
fn feeder_restart_preserves_the_current_controlled_quote() {
    assert_eq!(
        feeder_start_quote(None),
        (MOCK_PRICE.to_owned(), MOCK_VOLUME.to_owned())
    );
    assert_eq!(
        feeder_start_quote(Some(("1.080001".into(), "77.000000".into()))),
        ("1.080001".into(), "77.000000".into())
    );
    assert_eq!(
        feeder_restart_expected_rate(Some(("1.080001".into(), "77.000000".into()))),
        U256::from(1_080_001_u64)
    );
    assert_eq!(feeder_restart_expected_rate(None), EXPECTED_RATE);
}

#[test]
fn process_feeder_count_is_ceiling_two_thirds() {
    let expected = [0usize, 1, 2, 2, 3, 4, 4, 5, 6, 6, 7];
    for (active, expected_quorum) in expected.into_iter().enumerate() {
        assert_eq!(oracle_quorum(active), expected_quorum, "N={active}");
    }
}

#[test]
fn overlap_fixture_is_a_ab_ab_b_and_uses_two_stablecoin_sources_for_coen() {
    let pairs = (0..4).map(overlap_pairs).collect::<Vec<_>>();
    assert_eq!(pairs.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2, 2, 1]);
    assert_eq!(
        pairs[0][0]
            .sources
            .iter()
            .map(|source| source.quote)
            .collect::<Vec<_>>(),
        ["USDT", "USDC"]
    );
    assert_eq!(pairs[1][1].sources[0].volume, "10");
    assert_eq!(pairs[2][1].sources[0].volume, "20");
    assert_eq!(pairs[3][0].sources[0].volume, "30");
}
