use super::*;

#[test]
fn replay_receipt_requires_success_inside_exclusive_deadline() {
    let receipt = serde_json::json!({"blockNumber": "0xc4", "status": "0x1"});
    assert_eq!(
        super::timely_replay_receipt_height(&receipt, 197).unwrap(),
        196
    );
    for height in [197_u64, 212] {
        let receipt = serde_json::json!({"blockNumber": format!("0x{height:x}"), "status": "0x1"});
        assert!(super::timely_replay_receipt_height(&receipt, 197).is_err());
    }
    let failed_run = serde_json::json!({"blockNumber": "0xd4", "status": "0x0"});
    let error = super::timely_replay_receipt_height(&failed_run, 197).unwrap_err();
    assert!(error.to_string().contains("outside exclusive deadline 197"));
}

#[test]
fn replay_receipt_rejects_reverts_and_missing_or_malformed_evidence() {
    for receipt in [
        serde_json::json!({"blockNumber": "0xc4", "status": "0x0"}),
        serde_json::json!({"blockNumber": "0xc4"}),
        serde_json::json!({"status": "0x1"}),
        serde_json::json!({"blockNumber": "196", "status": "0x1"}),
        serde_json::json!({"blockNumber": "0xinvalid", "status": "0x1"}),
    ] {
        assert!(super::timely_replay_receipt_height(&receipt, 197).is_err());
    }
}

#[test]
fn case_one_dispatch_marker_requires_exact_job_and_production_event() {
    let job = B256::repeat_byte(0x31);
    let valid = format!(
            "2026-09-05T19:46:15.099744Z  INFO exex{{id=\"outbe-finalized\"}}: outbe_chain::ocomp_exex: embedded OCOMP computation started job_id={job:#x}"
        );
    assert_eq!(
        super::case_one_compute_started_line(&valid, job),
        Some(valid.as_str())
    );
    assert!(super::case_one_compute_started_line(&valid, B256::repeat_byte(0x32)).is_none());
    for invalid in [
        valid.replace(" INFO ", " WARN "),
        valid.replace("outbe_chain::ocomp_exex: ", "other_module: "),
        valid.replace("computation started", "local result arrived"),
        format!("{valid}0"),
        format!("{valid} reason=\"checkpoint_pruned\""),
    ] {
        assert!(
            super::case_one_compute_started_line(&invalid, job).is_none(),
            "{invalid}"
        );
    }
}

#[test]
fn retention_evidence_reads_the_node_consensus_storage_root() {
    assert_eq!(
        retention_journal_root(std::path::Path::new("/scenario/validator-0/data")),
        std::path::PathBuf::from("/scenario/validator-0/data/consensus/ocomp_retention")
    );
}

#[test]
fn completed_accountability_allows_only_monotonic_late_vote_extension() {
    let expected = completed_accountability();
    let mut extended = expected.clone();
    extended.slot_validator_indexes.push(3);
    extended.slot_first_signatures.push((3, vec![0xa3]));

    assert!(completed_accountability_is_preserved(&expected, &extended));

    let mut changed_quorum = extended.clone();
    changed_quorum.quorum_height = Some(93);
    assert!(!completed_accountability_is_preserved(
        &expected,
        &changed_quorum
    ));

    let mut replaced_signature = extended;
    replaced_signature.slot_first_signatures[0].1 = vec![0xff];
    assert!(!completed_accountability_is_preserved(
        &expected,
        &replaced_signature
    ));
}

#[test]
fn ordinary_ocomp_completion_accepts_the_first_canonical_quorum() {
    assert!(public_vote_set_matches(
        PublicVoteSetExpectation::AnyQuorum,
        &[0, 1, 2],
        3,
    ));
    assert!(!public_vote_set_matches(
        PublicVoteSetExpectation::AnyQuorum,
        &[0, 1],
        3,
    ));
    assert!(!public_vote_set_matches(
        PublicVoteSetExpectation::Exact(&[1, 2, 3]),
        &[0, 1, 2],
        3,
    ));
}

#[test]
fn dynamic_pre_restart_baseline_waits_for_all_three_job_b_votes() {
    assert!(!dynamic_pre_restart_vote_baseline_ready(2, 2, true));
    assert!(dynamic_pre_restart_vote_baseline_ready(2, 3, true));
    assert!(!dynamic_pre_restart_vote_baseline_ready(2, 3, false));
}

#[test]
fn capacity_population_submits_two_tributes_per_round() {
    assert_eq!(OCOMP_CAPACITY_SUBMISSION_CONCURRENCY, 2);
}
