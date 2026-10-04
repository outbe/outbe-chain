use super::*;
use alloy_primitives::{B256, U256};
use fixtures::{config, Action, RecordingIo};

mod fixtures;

#[tokio::test]
async fn unreadable_journal_stops_before_finalized_reads_or_promotion() {
    let io = RecordingIo::new().unwrap();
    io.journal
        .borrow_mut()
        .push_back(Err(eyre::eyre!("corrupt journal")));
    run_worker(&io, config()).await;
    assert_eq!(*io.actions.borrow(), [Action::Inspect]);
}

use fixtures::{context, finalized, finalized_view, security, status, submission, submitted};
use futures::FutureExt as _;
use outbe_node::tee_remote_session::LocalRegistryAdmissionError;

async fn assert_worker(io: RecordingIo, expected: &[Action], notified: bool) {
    let config = config();
    let notification = config.promoted.clone();
    run_worker(&io, config).await;
    assert_eq!(*io.actions.borrow(), expected);
    assert_eq!(notification.notified().now_or_never().is_some(), notified);
    assert!(
        io.journal.borrow().is_empty(),
        "journal reads must not be skipped"
    );
    assert!(
        io.status.borrow().is_empty(),
        "finalized reads must not be skipped"
    );
}

#[tokio::test]
async fn missing_and_promoted_journals_poll_without_reading_finalized_state() {
    let missing = RecordingIo::new().unwrap();
    missing.journal.borrow_mut().push_back(Ok(None));
    assert_worker(missing, &[Action::Inspect, Action::Inspect], false).await;
    let promoted = RecordingIo::new().unwrap();
    promoted.checkpoint(UpgradeJournalStateV1::Promoted {
        context: context(),
        security: security(),
        submission: submission(),
        finalized_height: 95,
        finalized_hash: B256::repeat_byte(27),
    });
    assert_worker(promoted, &[Action::Inspect, Action::Inspect], false).await;
}

#[tokio::test]
async fn terminal_checkpoint_stops_before_loading_manifest_or_finalized_state() {
    let io = RecordingIo::new().unwrap();
    io.checkpoint(UpgradeJournalStateV1::TerminalMissedCutoff {
        context: context(),
        finalized_height: 105,
        activation_height: 100,
    });
    assert_worker(io, &[Action::Inspect], false).await;
}

#[tokio::test]
async fn restart_after_installed_candidate_only_reconciles_the_promoted_checkpoint() {
    for fail in [None, Some(Action::Promoted)] {
        let mut io = RecordingIo::new().unwrap();
        let context = outbe_operator::tee::UpgradeContextV1 {
            candidate_manifest_hash: io.manifest.authorization_hash().unwrap(),
            ..context()
        };
        io.checkpoint(finalized(context));
        io.fail = fail;
        assert_worker(
            io,
            &[Action::Inspect, Action::Manifest, Action::Promoted],
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn finalized_recovery_promotes_and_records_before_notifying_restart() {
    let io = RecordingIo::new().unwrap();
    io.checkpoint(finalized(context()));
    *io.authorization.borrow_mut() = Some(Ok(finalized_view(105)));
    assert_worker(
        io,
        &[
            Action::Inspect,
            Action::Manifest,
            Action::Authorize,
            Action::Promote,
            Action::Promoted,
        ],
        true,
    )
    .await;
}

#[tokio::test]
async fn recovery_manifest_and_authority_errors_stop_without_checkpoint_or_notification() {
    let mut io = RecordingIo::new().unwrap();
    io.checkpoint(finalized(context()));
    io.fail = Some(Action::Manifest);
    assert_worker(io, &[Action::Inspect, Action::Manifest], false).await;
    let mut io = RecordingIo::new().unwrap();
    io.checkpoint(finalized(context()));
    io.manifest.node_id.reth_p2p_public = [0; 33];
    assert_worker(io, &[Action::Inspect, Action::Manifest], false).await;
    for failure in [
        LocalRegistryAdmissionError::ReplacementBindingMissing,
        LocalRegistryAdmissionError::Provider("authority unavailable".into()),
    ] {
        let io = RecordingIo::new().unwrap();
        io.checkpoint(finalized(context()));
        *io.authorization.borrow_mut() = Some(Err(failure));
        assert_worker(
            io,
            &[Action::Inspect, Action::Manifest, Action::Authorize],
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn submitted_finalized_binding_wins_over_a_crossed_activation_cutoff() {
    assert_worker(
        submitted().unwrap(),
        &[
            Action::Inspect,
            Action::Status,
            Action::Manifest,
            Action::Authorize,
            Action::Finalize(105, B256::repeat_byte(13)),
            Action::Promote,
            Action::Promoted,
        ],
        true,
    )
    .await;
}

#[tokio::test]
async fn durable_and_promotion_failures_stop_before_later_effects_or_notification() {
    let full = [
        Action::Inspect,
        Action::Status,
        Action::Manifest,
        Action::Authorize,
        Action::Finalize(105, B256::repeat_byte(13)),
        Action::Promote,
        Action::Promoted,
    ];
    for end in 5..=7 {
        let mut io = submitted().unwrap();
        io.fail = Some(full[end - 1]);
        assert_worker(io, &full[..end], false).await;
    }
    for (failure, expected) in [
        (
            Action::Promote,
            vec![
                Action::Inspect,
                Action::Manifest,
                Action::Authorize,
                Action::Promote,
            ],
        ),
        (
            Action::Promoted,
            vec![
                Action::Inspect,
                Action::Manifest,
                Action::Authorize,
                Action::Promote,
                Action::Promoted,
            ],
        ),
    ] {
        let mut io = RecordingIo::new().unwrap();
        io.checkpoint(finalized(context()));
        *io.authorization.borrow_mut() = Some(Ok(finalized_view(105)));
        io.fail = Some(failure);
        assert_worker(io, &expected, false).await;
    }
}

#[tokio::test]
async fn only_missing_submitted_binding_can_continue_to_deadline_handling() {
    let missing = submitted().unwrap();
    *missing.authorization.borrow_mut() =
        Some(Err(LocalRegistryAdmissionError::ReplacementBindingMissing));
    assert_worker(
        missing,
        &[
            Action::Inspect,
            Action::Status,
            Action::Manifest,
            Action::Authorize,
            Action::Missed(105, 100),
        ],
        false,
    )
    .await;
    let rejected = submitted().unwrap();
    *rejected.authorization.borrow_mut() = Some(Err(
        LocalRegistryAdmissionError::ReplacementAuthorization("rejected".into()),
    ));
    assert_worker(
        rejected,
        &[
            Action::Inspect,
            Action::Status,
            Action::Manifest,
            Action::Authorize,
        ],
        false,
    )
    .await;
}

#[tokio::test]
async fn matching_strict_upgrade_keeps_owner_recovery_open_after_deadline() {
    for (proposal, hash, expected) in [
        (
            1,
            B256::repeat_byte(12),
            vec![Action::Inspect, Action::Status, Action::Inspect],
        ),
        (
            0,
            B256::repeat_byte(12),
            vec![Action::Inspect, Action::Status, Action::Missed(100, 100)],
        ),
        (
            1,
            B256::repeat_byte(99),
            vec![Action::Inspect, Action::Status, Action::Missed(100, 100)],
        ),
    ] {
        let io = RecordingIo::new().unwrap();
        io.checkpoint(UpgradeJournalStateV1::CandidatePrepared { context: context() });
        let mut status = status(100);
        status.strict_upgrade.proposal_id = U256::from(proposal);
        status.strict_upgrade.successor_policy_hash = hash;
        io.status.borrow_mut().push_back(Ok(status));
        assert_worker(io, &expected, false).await;
    }
}

#[tokio::test]
async fn transient_status_error_retries_with_a_fresh_finalized_read() {
    let io = RecordingIo::new().unwrap();
    for _ in 0..2 {
        io.checkpoint(UpgradeJournalStateV1::CandidatePrepared { context: context() });
    }
    io.status
        .borrow_mut()
        .push_back(Err(LocalRegistryAdmissionError::Provider(
            "temporary".into(),
        )));
    io.status.borrow_mut().push_back(Ok(status(100)));
    assert_worker(
        io,
        &[
            Action::Inspect,
            Action::Status,
            Action::Inspect,
            Action::Status,
            Action::Missed(100, 100),
        ],
        false,
    )
    .await;
}

#[tokio::test]
async fn cutoff_record_failure_still_stops_and_never_requests_restart() {
    let mut io = RecordingIo::new().unwrap();
    io.checkpoint(UpgradeJournalStateV1::CandidatePrepared { context: context() });
    io.status.borrow_mut().push_back(Ok(status(100)));
    io.fail = Some(Action::Missed(100, 100));
    assert_worker(
        io,
        &[Action::Inspect, Action::Status, Action::Missed(100, 100)],
        false,
    )
    .await;
}

#[tokio::test]
async fn all_pre_submission_states_keep_waiting_before_activation() {
    for lifecycle in [
        UpgradeJournalStateV1::CandidatePrepared { context: context() },
        UpgradeJournalStateV1::KeyProvisioned {
            context: context(),
            sealed_root_hash: B256::repeat_byte(24),
        },
        UpgradeJournalStateV1::CandidateKeyReady {
            context: context(),
            security: security(),
        },
        UpgradeJournalStateV1::SubmissionPrepared {
            context: context(),
            security: security(),
            submission: submission(),
        },
    ] {
        let io = RecordingIo::new().unwrap();
        io.checkpoint(lifecycle);
        io.status.borrow_mut().push_back(Ok(status(99)));
        assert_worker(
            io,
            &[Action::Inspect, Action::Status, Action::Inspect],
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn staged_policy_failures_stop_before_submitted_authorization() {
    use outbe_primitives::tee_genesis_v1::{
        initial_tee_policy_v1, InitialTeeProfileV1, GRAMINE_DIRECT_DEV_CHAIN_ID,
    };
    let mut policy = initial_tee_policy_v1(
        InitialTeeProfileV1::GramineDirectDev,
        GRAMINE_DIRECT_DEV_CHAIN_ID,
        B256::repeat_byte(1),
    )
    .unwrap();
    policy.activation_height = 100;
    for failure in ["hash", "activation", "invalid-policy"] {
        let io = submitted().unwrap();
        let mut selected = policy.clone();
        {
            let mut journal = io.journal.borrow_mut();
            let UpgradeJournalStateV1::Submitted { context, .. } = &mut journal
                .front_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .lifecycle
            else {
                panic!("submitted fixture required")
            };
            context.successor_policy_hash = selected.policy_hash().unwrap();
            match failure {
                "hash" => context.successor_policy_hash = B256::repeat_byte(99),
                "activation" => context.activation_height = 101,
                "invalid-policy" => selected.policy_version = 0,
                _ => unreachable!(),
            }
        }
        io.status
            .borrow_mut()
            .front_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .staged_policy = Some(selected);
        assert_worker(io, &[Action::Inspect, Action::Status], false).await;
    }
}

#[tokio::test]
async fn submitted_manifest_error_stops_before_authorization() {
    let mut io = submitted().unwrap();
    io.fail = Some(Action::Manifest);
    assert_worker(
        io,
        &[Action::Inspect, Action::Status, Action::Manifest],
        false,
    )
    .await;
}

#[tokio::test]
async fn submitted_missing_binding_before_cutoff_waits_without_terminal_checkpoint() {
    let io = submitted().unwrap();
    *io.status.borrow_mut().front_mut().unwrap() = Ok(status(99));
    *io.authorization.borrow_mut() =
        Some(Err(LocalRegistryAdmissionError::ReplacementBindingMissing));
    assert_worker(
        io,
        &[
            Action::Inspect,
            Action::Status,
            Action::Manifest,
            Action::Authorize,
            Action::Inspect,
        ],
        false,
    )
    .await;
}
