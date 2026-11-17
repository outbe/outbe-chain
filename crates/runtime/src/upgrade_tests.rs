use super::*;
use alloy_primitives::{B256, U256};
use fixtures::{config, Action, RecordingIo};

#[path = "upgrade_test_fixtures.rs"]
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
use outbe_execution::tee_remote_session::LocalRegistryAdmissionError;

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
        let context = UpgradeContextV1 {
            candidate_manifest_hash: io.manifest.authorization_hash().unwrap(),
            ..context()
        };
        io.checkpoint(finalized(context));
        io.fail = fail;
        assert_worker(
            io,
            &if fail.is_none() {
                vec![
                    Action::Inspect,
                    Action::Manifest,
                    Action::Promoted,
                    Action::Inspect,
                ]
            } else {
                vec![Action::Inspect, Action::Manifest, Action::Promoted]
            },
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn finalized_recovery_promotes_and_records_without_requesting_restart() {
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
            Action::Inspect,
        ],
        false,
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
            Action::Inspect,
        ],
        false,
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
        initial_tee_policy_v1, InitialTeeProfileV1, NO_ATTESTATION_DEV_CHAIN_ID,
    };
    let mut policy = initial_tee_policy_v1(
        InitialTeeProfileV1::NoAttestationDev,
        NO_ATTESTATION_DEV_CHAIN_ID,
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

struct LivePromotionFixture {
    manifest: outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    view: outbe_tee::FinalizedRegistryViewV1,
    actions: std::cell::RefCell<Vec<&'static str>>,
    fail: bool,
}
impl UpgradePromotionIo for LivePromotionFixture {
    type Authorization = outbe_tee::FinalizedRegistryViewV1;
    fn inspect_journal(&self) -> eyre::Result<Option<UpgradeJournalSnapshotV1>> {
        Ok(None)
    }
    fn committed_manifest(
        &self,
    ) -> eyre::Result<outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1> {
        Ok(self.manifest.clone())
    }
    fn successor_status(
        &self,
    ) -> Result<
        outbe_execution::tee_remote_session::LocalFinalizedSuccessorStatusV1,
        LocalRegistryAdmissionError,
    > {
        unreachable!()
    }
    fn replacement_authorization(
        &self,
        node: &outbe_primitives::tee_attestation_v1::NodeIdV1,
    ) -> Result<ports::PromotionAuthorization<Self::Authorization>, LocalRegistryAdmissionError>
    {
        assert_eq!(node, &self.manifest.node_id);
        Ok(ports::PromotionAuthorization {
            authorization: self.view,
            view: self.view,
        })
    }
    fn promote_candidate(&self, authorization: &Self::Authorization) -> eyre::Result<()> {
        assert_eq!(authorization, &self.view);
        self.actions.borrow_mut().push("durable");
        if self.fail {
            return Err(
                crate::admission_authority::live::LiveTeeError::CandidateNotAdmitted.into(),
            );
        }
        Ok(())
    }
    fn record_finalized(&self, height: u64, hash: B256) -> eyre::Result<()> {
        assert_eq!(
            (height, hash),
            (self.view.block_number, self.view.block_hash)
        );
        self.actions.borrow_mut().push("finalized");
        Ok(())
    }
    fn record_promoted(&self) -> eyre::Result<()> {
        self.actions.borrow_mut().push("promoted");
        Ok(())
    }
    fn record_missed_cutoff(&self, _: u64, _: u64) -> eyre::Result<()> {
        unreachable!()
    }
}

#[tokio::test]
async fn live_upgrade_and_lease_guard_share_identity_anchor_and_context_without_restart() {
    use crate::admission::{TeeLeaseGuardAnchorsV1, TeeLeaseGuardConfigV1, TeeLeaseGuardGateV1};
    use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    for fail in [false, true] {
        let old = support::context(9);
        let candidate = support::context(10);
        let (provider, chain, identity) =
            support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
        let shared = support::SharedProvider(Arc::new(std::sync::Mutex::new(provider)));
        let authority = TeeAuthorityChannel::default();
        let mut watch = authority.subscribe();
        let anchors = TeeLeaseGuardAnchorsV1 {
            gate: TeeLeaseGuardGateV1::Armed,
            durable: None,
        };
        let owner = Arc::new(
            LiveTeeRuntimeV1::new(
                LiveTeeSnapshotV1 {
                    identity,
                    anchors,
                    context: old.clone(),
                },
                authority.clone(),
            )
            .unwrap(),
        );
        let notify = Arc::new(tokio::sync::Notify::new());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let guard = run_lease_guard(
            shared.clone(),
            (
                Arc::new(crate::OutbeRuntime::new()),
                Arc::new(crate::OutbeRuntime::new()),
            ),
            TeeLeaseGuardConfigV1 {
                chain,
                identity,
                anchors,
                finalization_notify: notify.clone(),
                armed: tokio::sync::watch::channel(false).0,
            },
            (authority.clone(), Some(owner.clone())),
            shutdown.clone(),
        );
        tokio::pin!(guard);
        tokio::select! { result = &mut guard => panic!("guard stopped: {result:?}"), () = support::wait_for_state(&mut watch, TeeAuthorityState::Armed { enclave_id: old.manifest().enclave_id().unwrap(), valid_until: 1_800_001_000 }) => {} }
        assert!(watch.snapshot().is_authority_open());
        let (successor, _, _) =
            support::provider(&candidate, 105, 1_800_000_000, 1_800_001_000, false);
        let head = successor.finalized_block_num_hash().unwrap().unwrap();
        let view = outbe_tee::FinalizedRegistryViewV1 {
            chain_id: U256::from(chain.chain_id).to_be_bytes(),
            genesis_hash: chain.genesis_hash,
            block_number: head.number,
            block_hash: head.hash,
            state_root: successor
                .sealed_header(head.number)
                .unwrap()
                .unwrap()
                .header()
                .inner
                .state_root,
            consensus_timestamp: 1_800_000_000,
        };
        let waiting = ports::LiveUpgradeIo {
            inner: LivePromotionFixture {
                manifest: old.manifest().clone(),
                view,
                actions: Default::default(),
                fail,
            },
            provider: shared.clone(),
            protocol: Arc::new(crate::OutbeRuntime::new()),
            chain,
            owner: owner.clone(),
        };
        let state_before = authority.snapshot();
        assert_eq!(super::poll_once(&waiting, &config()), CycleOutcome::Wait);
        assert!(waiting.inner.actions.borrow().is_empty());
        assert_eq!(authority.snapshot(), state_before);
        *shared.0.lock().unwrap() = successor;
        notify.notify_one();
        tokio::select! { result = &mut guard => panic!("guard died before promotion: {result:?}"), () = support::wait_for_state(&mut watch, TeeAuthorityState::SuspendedPolicyMismatch) => {} }
        assert!(guard.as_mut().now_or_never().is_none());

        owner.stage_candidate(candidate.clone()).unwrap();
        let _dispatch = outbe_tee::client_global::set_in_process_tee_context_for_test(Some((
            old.clone(),
            outbe_enclave::host::domain_in_process::dispatch_payload,
        )));
        let io = ports::LiveUpgradeIo {
            inner: LivePromotionFixture {
                manifest: old.manifest().clone(),
                view,
                actions: Default::default(),
                fail,
            },
            provider: shared.clone(),
            protocol: Arc::new(crate::OutbeRuntime::new()),
            chain,
            owner: owner.clone(),
        };
        let promoted = tokio::sync::Notify::new();
        assert_eq!(
            promotion::reconcile_submitted(&io, &promoted),
            Some(if fail {
                CycleOutcome::Stop
            } else {
                CycleOutcome::Wait
            })
        );
        assert!(
            promoted.notified().now_or_never().is_none(),
            "promotion never requests an execution restart"
        );
        if fail {
            let snapshot = owner.lock().unwrap();
            assert_eq!(snapshot.identity, identity);
            assert_eq!(snapshot.anchors, anchors);
            assert!(Arc::ptr_eq(&snapshot.context, &old));
            assert!(Arc::ptr_eq(
                &outbe_tee::in_process_tee_context().unwrap(),
                &old
            ));
            assert_eq!(
                authority.snapshot(),
                TeeAuthorityState::SuspendedPolicyMismatch
            );
            assert_eq!(*io.inner.actions.borrow(), ["finalized", "durable"]);
        } else {
            let snapshot = owner.lock().unwrap().clone();
            assert!(Arc::ptr_eq(&snapshot.context, &candidate));
            assert!(Arc::ptr_eq(
                &outbe_tee::in_process_tee_context().unwrap(),
                &candidate
            ));
            assert!(owner.staged_candidate().unwrap().is_none());
            assert_eq!(
                snapshot.identity.expected_enclave_id,
                Some(candidate.manifest().enclave_id().unwrap())
            );
            assert_eq!(
                snapshot
                    .anchors
                    .gate
                    .pending_anchor()
                    .unwrap()
                    .finalized_hash,
                head.hash
            );
            assert_eq!(
                *io.inner.actions.borrow(),
                ["finalized", "durable", "promoted"]
            );
            let armed = TeeAuthorityState::Armed {
                enclave_id: candidate.manifest().enclave_id().unwrap(),
                valid_until: 1_800_001_000,
            };
            support::wait_for_state(&mut watch, armed).await;
            notify.notify_one();
            assert!(guard.as_mut().now_or_never().is_none());
            assert_eq!(
                watch.snapshot(),
                TeeAuthorityState::Armed {
                    enclave_id: candidate.manifest().enclave_id().unwrap(),
                    valid_until: 1_800_001_000
                }
            );
            assert!(owner.lock().unwrap().anchors.gate.is_armed());
            *shared.0.lock().unwrap() =
                support::provider(&candidate, 106, 1_800_000_000, 1_800_001_000, true).0;
            notify.notify_one();
            tokio::select! { result = &mut guard => panic!("guard died after promotion: {result:?}"), () = support::wait_for_state(&mut watch, TeeAuthorityState::SuspendedJailed) => {} }
            assert!(guard.as_mut().now_or_never().is_none());
        }
        assert!(old.offer_key_ready().unwrap());
        assert!(candidate.offer_key_ready().unwrap());
        shutdown.cancel();
        assert_eq!(guard.await.unwrap(), None);
    }
}

#[tokio::test]
async fn successful_promotion_keeps_polling_without_promoting_the_active_candidate_again() {
    let io = RecordingIo::new().unwrap();
    io.checkpoint(finalized(context()));
    io.checkpoint(UpgradeJournalStateV1::Promoted {
        context: context(),
        security: security(),
        submission: submission(),
        finalized_height: 105,
        finalized_hash: B256::repeat_byte(13),
    });
    io.journal.borrow_mut().push_back(Ok(None));
    *io.authorization.borrow_mut() = Some(Ok(finalized_view(105)));
    assert_worker(
        io,
        &[
            Action::Inspect,
            Action::Manifest,
            Action::Authorize,
            Action::Promote,
            Action::Promoted,
            Action::Inspect,
            Action::Inspect,
            Action::Inspect,
        ],
        false,
    )
    .await;
}

#[test]
fn advanced_finalized_head_retries_promotion_with_a_fresh_view() {
    use crate::admission::{TeeLeaseGuardAnchorsV1, TeeLeaseGuardGateV1};
    use crate::admission_authority::{live::*, test_support as support};
    let old = support::context(9);
    let candidate = support::context(10);
    let (original, chain, identity) =
        support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                anchors: TeeLeaseGuardAnchorsV1 {
                    gate: TeeLeaseGuardGateV1::Armed,
                    durable: None,
                },
                context: old.clone(),
            },
            outbe_tee::TeeAuthorityChannel::default(),
        )
        .unwrap(),
    );
    owner
        .refresh(&original, &crate::OutbeRuntime::new(), &chain)
        .unwrap();
    let (authorized, _, _) =
        support::provider(&candidate, 105, 1_800_000_000, 1_800_001_000, false);
    let checkpoint = authorized.finalized_block_num_hash().unwrap().unwrap();
    let header = authorized.sealed_header(105).unwrap().unwrap();
    let view = outbe_tee::FinalizedRegistryViewV1 {
        chain_id: U256::from(chain.chain_id).to_be_bytes(),
        genesis_hash: chain.genesis_hash,
        block_number: 105,
        block_hash: checkpoint.hash,
        state_root: header.header().inner.state_root,
        consensus_timestamp: 1_800_000_000,
    };
    let (mut advanced, _, _) =
        support::provider(&candidate, 106, 1_800_000_001, 1_800_001_000, false);
    advanced
        .inner
        .add_header(checkpoint.hash, header.header().clone());
    advanced
        .historical_states
        .insert(checkpoint.hash, authorized.inner.clone());
    let fresh = advanced.finalized_block_num_hash().unwrap().unwrap();
    owner.stage_candidate(candidate.clone()).unwrap();
    let _dispatch = outbe_tee::client_global::set_in_process_tee_context_for_test(Some((
        old.clone(),
        outbe_enclave::host::domain_in_process::dispatch_payload,
    )));
    let mut io = ports::LiveUpgradeIo {
        inner: LivePromotionFixture {
            manifest: old.manifest().clone(),
            view,
            actions: Default::default(),
            fail: false,
        },
        provider: advanced,
        protocol: Arc::new(crate::OutbeRuntime::new()),
        chain,
        owner: owner.clone(),
    };
    let promoted = tokio::sync::Notify::new();
    assert_eq!(
        promotion::reconcile_submitted(&io, &promoted),
        Some(CycleOutcome::Wait)
    );
    assert_eq!(*io.inner.actions.borrow(), ["finalized"]);
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &old));
    io.inner.view = outbe_tee::FinalizedRegistryViewV1 {
        block_number: fresh.number,
        block_hash: fresh.hash,
        consensus_timestamp: 1_800_000_001,
        ..view
    };
    let context = UpgradeContextV1 {
        candidate_manifest_hash: candidate.manifest().authorization_hash().unwrap(),
        ..fixtures::context()
    };
    assert_eq!(
        promotion::recover_finalized(&io, &context, checkpoint, &promoted),
        CycleOutcome::Wait
    );
    assert_eq!(
        *io.inner.actions.borrow(),
        ["finalized", "durable", "promoted"]
    );
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &candidate));
    assert!(owner.authority.snapshot().is_authority_open());
    io.inner.view.chain_id = [0; 32];
    assert_eq!(
        promotion::reconcile_submitted(&io, &promoted),
        Some(CycleOutcome::Stop)
    );
    assert_eq!(
        io.inner
            .actions
            .borrow()
            .iter()
            .filter(|action| **action == "durable")
            .count(),
        1
    );
}

#[test]
fn restaged_candidate_cannot_be_overwritten_by_an_older_promotion() {
    use crate::admission::{
        LocalTeeAdmissionAnchorV1, TeeLeaseGuardAnchorsV1, TeeLeaseGuardGateV1,
    };
    use crate::admission_authority::{live::*, test_support as support};
    let old = support::context(9);
    let candidate = support::context(10);
    let newer = support::context(11);
    let (_, _, identity) = support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            anchors: TeeLeaseGuardAnchorsV1 {
                gate: TeeLeaseGuardGateV1::Armed,
                durable: None,
            },
            context: old.clone(),
        },
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    owner.stage_candidate(candidate.clone()).unwrap();
    owner.stage_candidate(newer.clone()).unwrap();
    let _dispatch = outbe_tee::client_global::set_in_process_tee_context_for_test(Some((
        old.clone(),
        outbe_enclave::host::domain_in_process::dispatch_payload,
    )));
    let result = owner.promote(
        candidate,
        LocalTeeAdmissionAnchorV1 {
            finalized_height: 8,
            finalized_hash: B256::repeat_byte(8),
        },
        outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready {
            valid_until: 1_800_001_000,
        },
        B256::from(old.network_encryption_public().unwrap()),
        || panic!("stale candidate must not have durable effects"),
    );
    assert!(matches!(result, Err(LiveTeeError::CandidateMismatch)));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &old));
    assert!(Arc::ptr_eq(
        &owner.staged_candidate().unwrap().unwrap(),
        &newer
    ));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &old
    ));
}
