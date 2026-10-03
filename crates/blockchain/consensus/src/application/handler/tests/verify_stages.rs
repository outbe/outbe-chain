use super::*;
use crate::application::handler::verification::VerifyTask;
use futures::{FutureExt as _, StreamExt as _};
use outbe_primitives::projection::ExecutionReadBudget;
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::system_tx::SystemTxInputV2;

fn proposed_block(parent: Option<&ConsensusBlock>, malformed_phase1: bool) -> ConsensusBlock {
    let parent_hash = parent.map_or(B256::ZERO, ConsensusBlock::block_hash);
    let mut raw = if parent.is_some() && !malformed_phase1 {
        crate::test_fixtures::block_with_system_inputs(
            &OutbeEvmSigner::from_secret_bytes([1; 32]).unwrap(),
            2,
            parent_hash,
            Bytes::new(),
            vec![
                SystemTxInputV2::CertifiedParentAccounting {
                    metadata: crate::test_fixtures::finalized_metadata(parent_hash),
                },
                SystemTxInputV2::LateFinalizeCredits {
                    artifact: Default::default(),
                },
                SystemTxInputV2::CycleTick,
                SystemTxInputV2::RewardsGemDelivery,
                SystemTxInputV2::OracleSlashWindow,
                SystemTxInputV2::HookEvents,
            ],
            outbe_primitives::chain::CHAIN_ID,
        )
        .into_inner()
        .into_block()
    } else {
        crate::test_fixtures::block_with_number_and_parent(
            if parent.is_some() { 2 } else { 1 },
            parent_hash,
        )
        .into_inner()
        .into_block()
    };
    raw.header.inner.transactions_root =
        alloy_consensus::proofs::calculate_transaction_root(&raw.body.transactions);
    raw.header.inner.timestamp = 3;
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw))
}

fn verify_context(parent: Digest) -> crate::application::ingress::SimplexContext {
    let (keys, _) = crate::test_fixtures::participants();
    crate::application::ingress::SimplexContext {
        round: Round::new(Epoch::new(0), View::new(2)),
        parent: (View::new(1), parent),
        leader: keys[0].public_key(),
    }
}

#[derive(Clone, Copy, Debug)]
enum DecisionCase {
    Valid,
    Invalid,
    Cancelled,
    StaleEpoch,
    Unavailable,
}

#[test]
fn verification_delegates_execution_without_canonicalizing_the_candidate() {
    for case in [
        DecisionCase::Valid,
        DecisionCase::Invalid,
        DecisionCase::Cancelled,
        DecisionCase::StaleEpoch,
        DecisionCase::Unavailable,
    ] {
        commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
            |context| async move {
                verify_decision_case(context, case).await;
            },
        );
    }
}

async fn verify_decision_case(
    context: commonware_runtime::deterministic::Context,
    case: DecisionCase,
) {
    use crate::executor::ingress::{Message, VerificationOutcome};
    let clock = context.child("decision");
    let (marshal, keepalive, actor) = start_marshal_without_available_block(context).await;
    let mut shared = finalizer_test_shared(&clock, marshal, HybridSchemeProvider::new());
    let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
    shared.shared.engine = super::super::EngineHandle::new(engine_tx);
    let (executor_tx, mut executor_rx) = futures::channel::mpsc::unbounded();
    shared.shared.executor_mailbox = crate::executor::Mailbox::from_sender(executor_tx);
    let block = proposed_block(None, false);
    shared
        .block_cache
        .insert_bounded(block.digest(), block.clone());
    let request = verify_context(Digest::ZERO);
    let round = request.round;
    let budget = ExecutionReadBudget::new();
    let (response, receiver) = oneshot::channel();
    let mut receiver = Some(receiver);
    let verify = shared.handle_verify(
        &clock,
        VerifyTask {
            context: request,
            payload_digest: block.digest(),
            response,
            execution_read_budget: budget.clone(),
        },
    );
    let executor = async {
        let Message::PendingParent(parent) = executor_rx.next().await.unwrap() else {
            panic!("the consensus parent must be recorded separately");
        };
        assert_eq!(parent.digest, Digest::ZERO);
        let Message::VerifyBlock(execution) = executor_rx.next().await.unwrap() else {
            panic!("application must delegate validity to executor");
        };
        assert_eq!(execution.request.block.digest(), block.digest());
        assert!(receiver.as_mut().unwrap().now_or_never().is_none());
        if matches!(case, DecisionCase::Cancelled) {
            drop(receiver.take());
            while !budget.is_cancelled() {
                clock.sleep(Duration::from_millis(1)).await;
            }
            assert!(execution.response.is_canceled());
            return;
        }
        if matches!(case, DecisionCase::StaleEpoch) {
            shared.epoch_fence.advance_epoch(Epoch::new(1));
        }
        let outcome = match case {
            DecisionCase::Invalid => VerificationOutcome::Invalid,
            DecisionCase::Unavailable => VerificationOutcome::Unavailable,
            _ => VerificationOutcome::Valid,
        };
        execution.response.send(outcome).unwrap();
        if matches!(case, DecisionCase::StaleEpoch | DecisionCase::Unavailable) {
            clock.sleep(Duration::from_millis(10)).await;
            assert!(
                receiver.as_mut().unwrap().now_or_never().is_none(),
                "local inability must abstain"
            );
            drop(receiver.take());
        }
    };
    let (result, ()) = futures::join!(verify, executor);
    result.unwrap();
    if let Some(receiver) = receiver {
        assert_eq!(receiver.await.unwrap(), matches!(case, DecisionCase::Valid));
    }
    let gate = shared
        .publication
        .certification_gate(&shared.marshal_mailbox, round, block.digest())
        .unwrap();
    assert!(
        gate.await,
        "storage must complete independently of verdict and cancellation"
    );
    assert_eq!(
        shared
            .marshal_mailbox
            .get_verified(round)
            .await
            .unwrap()
            .digest(),
        block.digest()
    );
    assert_eq!(shared.finalization_view.timestamp_floor(), 0);
    assert!(
        executor_rx.next().now_or_never().is_none(),
        "verification must never select HEAD"
    );
    assert!(
        engine_rx.try_recv().is_err(),
        "application must not execute verification itself"
    );
    drop(keepalive);
    actor.abort();
    let _ = actor.await;
}

#[derive(Clone, Copy, Debug)]
enum EarlyCase {
    StaleEpoch,
    WrongParent,
    BeyondBoundary,
    MalformedPhase1,
    InvalidLiveHeader,
    CancelledProjection,
    MissingEpochAnchor,
}

#[test]
fn verify_prechecks_reject_or_withhold_before_engine_work() {
    for case in [
        EarlyCase::StaleEpoch,
        EarlyCase::WrongParent,
        EarlyCase::BeyondBoundary,
        EarlyCase::MalformedPhase1,
        EarlyCase::InvalidLiveHeader,
        EarlyCase::CancelledProjection,
        EarlyCase::MissingEpochAnchor,
    ] {
        commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
            |context| async move {
                let clock = context.child("verify_prechecks");
                let (marshal, keepalive, actor) =
                    start_marshal_without_available_block(context).await;
                let mut shared =
                    finalizer_test_shared(&clock, marshal, HybridSchemeProvider::new());
                let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
                shared.shared.engine = super::super::EngineHandle::new(engine_tx);
                let parent = consensus_block_with_timestamp(0x72, 1, 2_000);
                let block = proposed_block(Some(&parent), true);
                shared
                    .block_cache
                    .insert_bounded(parent.digest(), parent.clone());
                shared
                    .block_cache
                    .insert_bounded(block.digest(), block.clone());
                let mut request = verify_context(parent.digest());
                let budget = ExecutionReadBudget::new();
                let (response, receiver) = oneshot::channel();
                let mut receiver = Some(receiver);
                match case {
                    EarlyCase::StaleEpoch => {
                        shared.epoch_fence.advance_epoch(Epoch::new(1));
                        shared.block_cache.get_and_remove(&block.digest());
                    }
                    EarlyCase::WrongParent => request.parent.1 = Digest(B256::ZERO),
                    EarlyCase::BeyondBoundary => {
                        shared.epoch_fence.arm_activation_boundary(Epoch::new(0), 1)
                    }
                    EarlyCase::InvalidLiveHeader => {
                        shared.shared.proposer_evm_address = Some(Address::repeat_byte(1))
                    }
                    EarlyCase::CancelledProjection => {
                        // Use height 1 so Phase 1 is optional; otherwise prechecks reject first.
                        let block = proposed_block(None, false);
                        shared
                            .block_cache
                            .insert_bounded(block.digest(), block.clone());
                        request.parent.1 = Digest(B256::ZERO);
                        shared
                            ._projection_publisher
                            .publish(ProjectionStatus::CatchingUp { checkpoint: None });
                        drop(receiver.take());
                        shared
                            .handle_verify(
                                &clock,
                                VerifyTask {
                                    context: request,
                                    payload_digest: block.digest(),
                                    response,
                                    execution_read_budget: budget.clone(),
                                },
                            )
                            .await
                            .unwrap();
                        assert!(budget.is_cancelled());
                        assert!(engine_rx.try_recv().is_err());
                        drop(keepalive);
                        actor.abort();
                        let _ = actor.await;
                        return;
                    }
                    EarlyCase::MissingEpochAnchor => {
                        shared.epoch_fence.advance_epoch(Epoch::new(1));
                        request.round = Round::new(Epoch::new(1), View::new(1));
                        request.parent.0 = View::new(0);
                    }
                    EarlyCase::MalformedPhase1 => {}
                }
                let verify = shared.handle_verify(
                    &clock,
                    VerifyTask {
                        context: request,
                        payload_digest: block.digest(),
                        response,
                        execution_read_budget: budget,
                    },
                );
                if matches!(case, EarlyCase::MissingEpochAnchor) {
                    let cancel = async {
                        clock
                            .sleep(
                                super::super::GENESIS_ANCHOR_WAIT_TIMEOUT
                                    + Duration::from_millis(10),
                            )
                            .await;
                        assert!(
                            receiver.as_mut().unwrap().now_or_never().is_none(),
                            "missing local anchor must abstain"
                        );
                        drop(receiver.take());
                    };
                    let (result, ()) = futures::join!(verify, cancel);
                    result.unwrap();
                } else {
                    verify.await.unwrap();
                    assert!(!receiver.take().unwrap().await.unwrap(), "{case:?}");
                }
                assert!(engine_rx.try_recv().is_err(), "{case:?}");
                assert_eq!(shared.finalization_view.timestamp_floor(), 0);
                drop(keepalive);
                actor.abort();
                let _ = actor.await;
            },
        );
    }
}

#[test]
fn live_executor_retries_execution_while_candidate_storage_remains_independent() {
    commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
        |context| async move {
            use reth_ethereum::node::api::BeaconEngineMessage;
            let clock = context.child("live_verify");
            let (marshal, keepalive, marshal_actor) =
                start_marshal_without_available_block(context).await;
            let mut shared =
                finalizer_test_shared(&clock, marshal.clone(), HybridSchemeProvider::new());
            let (tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
            let engine = super::super::EngineHandle::new(tx);
            let (executor, mailbox) = crate::executor::actor::ExecutorActor::new(
                clock.child("executor"),
                engine,
                B256::ZERO,
                0,
                B256::ZERO,
                shared.projection_readiness.clone(),
                None,
            );
            let executor_actor =
                executor.start(marshal.clone(), commonware_consensus::types::Height::zero());
            shared.shared.executor_mailbox = mailbox;
            let block = proposed_block(None, false);
            shared
                .block_cache
                .insert_bounded(block.digest(), block.clone());
            let request = verify_context(Digest::ZERO);
            let round = request.round;
            let (response, mut receiver) = oneshot::channel();
            let verify = shared.handle_verify(
                &clock,
                VerifyTask {
                    context: request,
                    payload_digest: block.digest(),
                    response,
                    execution_read_budget: ExecutionReadBudget::new(),
                },
            );
            let execution = async {
                for status in [PayloadStatusEnum::Syncing, PayloadStatusEnum::Valid] {
                    let BeaconEngineMessage::NewPayload { payload, tx } =
                        engine_rx.recv().await.unwrap()
                    else {
                        panic!("candidate verification must not send forkchoice updates");
                    };
                    assert_eq!(
                        reth_node_builder::ExecutionPayload::block_hash(&payload),
                        block.block_hash()
                    );
                    if status == PayloadStatusEnum::Valid {
                        assert!(receiver.try_recv().is_err(), "SYNCING is not a verdict");
                        assert_eq!(
                            marshal.get_verified(round).await.unwrap().digest(),
                            block.digest(),
                            "candidate storage must proceed while execution is pending"
                        );
                    }
                    tx.send(Ok(PayloadStatus::from_status(status))).unwrap();
                }
            };
            let (result, ()) = futures::join!(verify, execution);
            result.unwrap();
            assert!(receiver.await.unwrap());
            assert!(
                shared
                    .publication
                    .certification_gate(&marshal, round, block.digest())
                    .unwrap()
                    .await
            );
            assert!(engine_rx.try_recv().is_err());
            assert_eq!(shared.finalization_view.timestamp_floor(), 0);
            executor_actor.abort();
            let _ = executor_actor.await;
            drop(keepalive);
            marshal_actor.abort();
            let _ = marshal_actor.await;
        },
    );
}

#[test]
fn cancellation_while_resolving_parent_preserves_available_candidate() {
    let (digest, checkpoint) =
        commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30))
            .start_and_recover(|context| async move {
                let clock = context.child("missing_parent");
                let fixture = super::certification::CertificationFixture::open(
                    &context,
                    "cancelled-peer-candidate",
                )
                .await;
                let shared = finalizer_test_shared(
                    &clock,
                    fixture.marshal.clone(),
                    HybridSchemeProvider::new(),
                );
                let missing_parent = proposed_block(None, false);
                let block = proposed_block(Some(&missing_parent), false);
                shared
                    .block_cache
                    .insert_bounded(block.digest(), block.clone());
                let request = verify_context(missing_parent.digest());
                let round = request.round;
                let budget = ExecutionReadBudget::new();
                let (response, receiver) = oneshot::channel();
                let verify = shared.handle_verify(
                    &clock,
                    VerifyTask {
                        context: request,
                        payload_digest: block.digest(),
                        response,
                        execution_read_budget: budget.clone(),
                    },
                );
                let cancel = async {
                    let gate = loop {
                        if let Some(gate) = shared.publication.certification_gate(
                            &shared.marshal_mailbox,
                            round,
                            block.digest(),
                        ) {
                            break gate;
                        }
                        clock.sleep(Duration::from_millis(1)).await;
                    };
                    assert!(
                        gate.await,
                        "candidate persistence must not wait for the unavailable parent"
                    );
                    assert!(
                        receiver.now_or_never().is_none(),
                        "parent resolution still prevents a verdict"
                    );
                };
                let (result, ()) = futures::join!(verify, cancel);
                result.unwrap();
                assert!(budget.is_cancelled());
                assert_eq!(
                    shared
                        .marshal_mailbox
                        .get_verified(round)
                        .await
                        .unwrap()
                        .digest(),
                    block.digest()
                );
                block.digest()
            });
    commonware_runtime::deterministic::Runner::from(checkpoint).start(|context| async move {
        use commonware_consensus::CertifiableAutomaton as _;
        let mut fixture =
            super::certification::CertificationFixture::open(&context, "cancelled-peer-candidate")
                .await;
        let round = Round::new(Epoch::new(0), View::new(2));
        assert!(
            fixture.app.certify(round, digest).await.await.unwrap(),
            "restart must recover the exact durable candidate without a verification verdict"
        );
        assert_eq!(
            fixture.marshal.get_verified(round).await.unwrap().digest(),
            digest
        );
    });
}

#[test]
fn epoch_parent_mismatch_rejects_without_waiting_for_an_unavailable_candidate() {
    commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
        |context| async move {
            let clock = context.child("wrong_boundary_parent");
            let (marshal, keepalive, actor) = start_marshal_without_available_block(context).await;
            let shared = finalizer_test_shared(&clock, marshal, HybridSchemeProvider::new());
            shared.epoch_fence.advance_epoch(Epoch::new(1));
            {
                let mut view = shared.finalization_view.write();
                *view = crate::finalization::state::FinalizationView::from_recovered(
                    B256::repeat_byte(0x64),
                    1,
                    Some(Round::new(Epoch::new(0), View::new(7))),
                );
            }
            let mut request = verify_context(Digest(B256::repeat_byte(0x65)));
            request.round = Round::new(Epoch::new(1), View::new(1));
            request.parent.0 = View::new(0);
            let (response, receiver) = oneshot::channel();
            let verify = shared.handle_verify(
                &clock,
                VerifyTask {
                    context: request,
                    payload_digest: Digest(B256::repeat_byte(0x66)),
                    response,
                    execution_read_budget: ExecutionReadBudget::new(),
                },
            );
            let futures::future::Either::Left(((result, verdict), _)) = futures::future::select(
                Box::pin(async { futures::join!(verify, receiver) }),
                Box::pin(clock.sleep(Duration::from_millis(1))),
            )
            .await
            else {
                panic!("a known invalid parent must reject before payload resolution timeout");
            };
            result.unwrap();
            assert!(!verdict.unwrap());
            drop(keepalive);
            actor.abort();
            let _ = actor.await;
        },
    );
}
