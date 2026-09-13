use super::*;

#[test]
fn canonical_status_matrix_is_exhaustive() {
    assert_eq!(
        classify_canonical_job(OcompJobStatus::AwaitingFinality, false).unwrap(),
        CanonicalJobDispositionV1::AwaitingFinality
    );
    assert_eq!(
        classify_canonical_job(OcompJobStatus::AwaitingFinality, true).unwrap(),
        CanonicalJobDispositionV1::FinalizedAwaitingOpen
    );
    assert_eq!(
        classify_canonical_job(OcompJobStatus::VotingOpen, true).unwrap(),
        CanonicalJobDispositionV1::VotingOpen
    );
    assert_eq!(
        classify_canonical_job(OcompJobStatus::Completed, true).unwrap(),
        CanonicalJobDispositionV1::Completed
    );
    for (status, reason) in [
        (OcompJobStatus::Expired, EmbeddedTerminalReasonV1::Expired),
        (OcompJobStatus::Failed, EmbeddedTerminalReasonV1::Failed),
    ] {
        for has_finalized_job in [false, true] {
            assert_eq!(
                classify_canonical_job(status, has_finalized_job).unwrap(),
                CanonicalJobDispositionV1::Closed {
                    reason,
                    has_finalized_job,
                }
            );
        }
    }
    for (status, has_finalized_job) in [
        (OcompJobStatus::VotingOpen, false),
        (OcompJobStatus::Completed, false),
    ] {
        assert!(classify_canonical_job(status, has_finalized_job).is_err());
    }
}

#[test]
fn async_outcome_projection_is_exact_or_retired() {
    let mut jobs = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::Validator);
    let generation = jobs.observe_job(B256::repeat_byte(0x71), 70).unwrap();
    let other_generation = jobs.observe_job(B256::repeat_byte(0x72), 71).unwrap();
    assert_eq!(
        classify_async_outcome_projection(Some(generation), Some(generation)).unwrap(),
        AsyncOutcomeProjectionV1::Active
    );
    assert_eq!(
        classify_async_outcome_projection(None, None).unwrap(),
        AsyncOutcomeProjectionV1::CheckpointPruned
    );
    for (runtime, reducer) in [
        (Some(generation), None),
        (None, Some(generation)),
        (Some(generation), Some(other_generation)),
    ] {
        assert!(classify_async_outcome_projection(runtime, reducer).is_err());
    }
}

#[test]
fn canonical_expiry_closes_without_export_ack_but_live_jobs_do_not() {
    assert!(request_projection_is_closed(
        false,
        Some(EmbeddedJobStateV1::Closed),
        Some(EmbeddedTerminalReasonV1::Expired),
    ));
    for state in [
        EmbeddedJobStateV1::Computing,
        EmbeddedJobStateV1::WaitAtDeadline,
        EmbeddedJobStateV1::LocalReady,
    ] {
        assert!(!request_projection_is_closed(false, Some(state), None));
        assert!(!request_projection_is_closed(true, Some(state), None));
    }
    assert!(request_projection_is_closed(
        true,
        Some(EmbeddedJobStateV1::Closed),
        Some(EmbeddedTerminalReasonV1::Failed),
    ));
}

#[test]
fn only_canonical_expiry_accepts_released_retention_without_export_authority() {
    assert_eq!(
        released_export_authority_for_status(OcompJobStatus::Expired, None).unwrap(),
        None
    );
    for status in [OcompJobStatus::Completed, OcompJobStatus::Failed] {
        assert!(released_export_authority_for_status(status, None).is_err());
    }
}

#[test]
fn expired_or_pruned_compute_is_rejected_before_persistence() {
    assert_eq!(
        ignored_compute_result_reason(
            AsyncOutcomeProjectionV1::Active,
            Some(EmbeddedTerminalReasonV1::Expired),
        ),
        Some("expired")
    );
    assert_eq!(
        ignored_compute_result_reason(AsyncOutcomeProjectionV1::CheckpointPruned, None),
        Some("checkpoint_pruned")
    );
    assert_eq!(
        ignored_compute_result_reason(
            AsyncOutcomeProjectionV1::Active,
            Some(EmbeddedTerminalReasonV1::Completed),
        ),
        None
    );
}

#[test]
fn canonical_restore_policy_never_runs_before_open_or_after_close() {
    for disposition in [
        CanonicalJobDispositionV1::AwaitingFinality,
        CanonicalJobDispositionV1::FinalizedAwaitingOpen,
        CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Expired,
            has_finalized_job: true,
        },
        CanonicalJobDispositionV1::Closed {
            reason: EmbeddedTerminalReasonV1::Failed,
            has_finalized_job: true,
        },
    ] {
        for policy in [
            EmbeddedNodePolicyV1::Validator,
            EmbeddedNodePolicyV1::FullNode,
        ] {
            assert_eq!(
                local_result_restore_policy(disposition, policy, true, true, false),
                LocalResultRestorePolicyV1::Never
            );
        }
    }
}

#[test]
fn canonical_restore_policy_encodes_effect_order() {
    assert_eq!(
        local_result_restore_policy(
            CanonicalJobDispositionV1::VotingOpen,
            EmbeddedNodePolicyV1::Validator,
            true,
            false,
            false,
        ),
        LocalResultRestorePolicyV1::BeforeCompute
    );
    assert_eq!(
        local_result_restore_policy(
            CanonicalJobDispositionV1::VotingOpen,
            EmbeddedNodePolicyV1::FullNode,
            false,
            false,
            false,
        ),
        LocalResultRestorePolicyV1::BeforeCompute,
        "the FinalizedAwaitingOpen to VotingOpen transition restores before compute"
    );
    assert_eq!(
        local_result_restore_policy(
            CanonicalJobDispositionV1::VotingOpen,
            EmbeddedNodePolicyV1::Validator,
            false,
            true,
            true,
        ),
        LocalResultRestorePolicyV1::BeforeCompute
    );
    assert_eq!(
        local_result_restore_policy(
            CanonicalJobDispositionV1::Completed,
            EmbeddedNodePolicyV1::FullNode,
            true,
            false,
            false,
        ),
        LocalResultRestorePolicyV1::AfterCanonicalCompleted
    );
    assert_eq!(
        local_result_restore_policy(
            CanonicalJobDispositionV1::Completed,
            EmbeddedNodePolicyV1::Validator,
            true,
            true,
            false,
        ),
        LocalResultRestorePolicyV1::Never
    );
    assert_eq!(
        local_result_restore_policy(
            CanonicalJobDispositionV1::VotingOpen,
            EmbeddedNodePolicyV1::FullNode,
            false,
            false,
            true,
        ),
        LocalResultRestorePolicyV1::Never
    );
}

#[test]
fn vote_eligibility_retries_only_unavailable_authority() {
    use outbe_node::ocomp::retention::OcompSnapshotEligibilityV1;

    let pending = advance_vote_eligibility(
        LocalVoteEligibilityV1::Pending,
        OcompSnapshotEligibilityV1::Unavailable {
            detail: "provider lag".to_owned(),
        },
    )
    .unwrap();
    assert_eq!(pending, LocalVoteEligibilityV1::Pending);
    assert_eq!(
        advance_vote_eligibility(pending, OcompSnapshotEligibilityV1::Eligible).unwrap(),
        LocalVoteEligibilityV1::Eligible
    );

    let not_member = advance_vote_eligibility(
        LocalVoteEligibilityV1::Pending,
        OcompSnapshotEligibilityV1::NotMember,
    )
    .unwrap();
    assert_eq!(not_member, LocalVoteEligibilityV1::NotMember);
    assert_eq!(
        advance_vote_eligibility(not_member, OcompSnapshotEligibilityV1::Eligible).unwrap(),
        LocalVoteEligibilityV1::NotMember,
        "exact non-membership is a terminal abstention"
    );
    assert!(advance_vote_eligibility(
        LocalVoteEligibilityV1::Pending,
        OcompSnapshotEligibilityV1::Corrupt {
            detail: "binding mismatch".to_owned(),
        },
    )
    .is_err());
}
