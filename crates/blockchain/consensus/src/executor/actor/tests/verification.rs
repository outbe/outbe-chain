use super::super::verification::VerificationWork;
use super::*;
use crate::application::epoch_boundary::ApplicationEpochFence;
use crate::executor::ingress::{
    PendingParent, VerificationOutcome, VerificationRequest, VerifyBlock,
};
use commonware_consensus::types::{Epoch, Round, View};
use commonware_runtime::deterministic;
use futures::FutureExt as _;
use outbe_primitives::projection::ExecutionReadBudget;

struct EngineStub {
    handle: ConsensusEngineHandle<outbe_primitives::OutbePayloadTypes>,
    rx: tokio::sync::mpsc::UnboundedReceiver<
        BeaconEngineMessage<outbe_primitives::OutbePayloadTypes>,
    >,
}

impl EngineStub {
    fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            handle: ConsensusEngineHandle::new(tx),
            rx,
        }
    }
}

fn linked(number: u64, parent_hash: B256, seed: u8) -> Arc<ConsensusBlock> {
    let mut block = Block::default();
    block.header.number = number;
    block.header.parent_hash = parent_hash;
    block.header.extra_data = Bytes::from(vec![seed]);
    Arc::new(ConsensusBlock::from_sealed(SealedBlock::seal_slow(
        block.map_header(OutbeHeader::new),
    )))
}

fn request(block: Arc<ConsensusBlock>, parent: Option<Arc<ConsensusBlock>>) -> VerificationRequest {
    VerificationRequest {
        round: Round::new(Epoch::new(0), View::new(4)),
        block,
        parent,
        epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
        execution_read_budget: ExecutionReadBudget::new(),
    }
}

fn enqueue(
    work: &mut VerificationWork,
    request: VerificationRequest,
) -> (
    futures::channel::oneshot::Receiver<VerificationOutcome>,
    ExecutionReadBudget,
) {
    let budget = request.execution_read_budget.clone();
    let (response, receiver) = futures::channel::oneshot::channel();
    work.queue(VerifyBlock { request, response });
    (receiver, budget)
}

async fn probe(
    work: &mut VerificationWork,
    clock: &deterministic::Context,
    engine: &mut EngineStub,
    script: (Digest, PayloadStatusEnum),
) -> eyre::Result<Option<(Height, Digest)>> {
    let finalized = (Height::zero(), Digest::ZERO);
    work.reconcile(finalized);
    work.schedule(&engine.handle);
    let event = work.next_event();
    let reply = async {
        let BeaconEngineMessage::NewPayload { payload, tx } = engine.rx.recv().await.unwrap()
        else {
            panic!("verification may only deliver a payload, never select HEAD");
        };
        assert_eq!(
            reth_node_builder::ExecutionPayload::block_hash(&payload),
            script.0 .0
        );
        tx.send(Ok(PayloadStatus::from_status(script.1))).unwrap();
    };
    let (event, ()) = futures::join!(event, reply);
    work.handle_event(event, clock, finalized)
}

#[test]
fn candidate_validity_is_independent_of_head_selection() {
    for (status, verdict) in [
        (PayloadStatusEnum::Valid, VerificationOutcome::Valid),
        (
            PayloadStatusEnum::Invalid {
                validation_error: "invalid candidate".into(),
            },
            VerificationOutcome::Invalid,
        ),
        (
            PayloadStatusEnum::Accepted,
            VerificationOutcome::Unavailable,
        ),
    ] {
        deterministic::Runner::default().start(|clock| async move {
            let mut engine = EngineStub::new();
            let mut work = VerificationWork::default();
            let block = linked(1, B256::ZERO, 1);
            let (receiver, _) = enqueue(&mut work, request(block.clone(), None));
            assert_eq!(
                probe(&mut work, &clock, &mut engine, (block.digest(), status))
                    .await
                    .unwrap(),
                None
            );
            assert_eq!(receiver.await.unwrap(), verdict);
        });
    }
}

#[test]
fn syncing_walk_executes_the_parent_then_reprobes_the_candidate() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let parent = linked(1, B256::ZERO, 2);
        let block = linked(2, parent.block_hash(), 3);
        let (receiver, _) = enqueue(&mut work, request(block.clone(), Some(parent.clone())));
        assert!(probe(
            &mut work,
            &clock,
            &mut engine,
            (block.digest(), PayloadStatusEnum::Syncing)
        )
        .await
        .unwrap()
        .is_none());
        assert!(probe(
            &mut work,
            &clock,
            &mut engine,
            (parent.digest(), PayloadStatusEnum::Valid)
        )
        .await
        .unwrap()
        .is_none());
        assert!(probe(
            &mut work,
            &clock,
            &mut engine,
            (block.digest(), PayloadStatusEnum::Valid)
        )
        .await
        .unwrap()
        .is_none());
        assert_eq!(receiver.await.unwrap(), VerificationOutcome::Valid);
    });
}

#[test]
fn an_invalid_ancestor_rejects_the_candidate() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let parent = linked(1, B256::ZERO, 4);
        let block = linked(2, parent.block_hash(), 5);
        let (receiver, _) = enqueue(&mut work, request(block.clone(), Some(parent.clone())));
        probe(
            &mut work,
            &clock,
            &mut engine,
            (block.digest(), PayloadStatusEnum::Syncing),
        )
        .await
        .unwrap();
        probe(
            &mut work,
            &clock,
            &mut engine,
            (
                parent.digest(),
                PayloadStatusEnum::Invalid {
                    validation_error: "invalid ancestor".into(),
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(receiver.await.unwrap(), VerificationOutcome::Invalid);
    });
}

#[test]
fn missing_or_mismatched_ancestry_never_votes_false() {
    for parent in [
        None,
        Some(linked(1, B256::ZERO, 9)),
        Some(linked(3, B256::ZERO, 8)),
    ] {
        deterministic::Runner::default().start(|clock| async move {
            let mut engine = EngineStub::new();
            let mut work = VerificationWork::default();
            let block = linked(2, B256::repeat_byte(0x71), 7);
            let (receiver, _) = enqueue(&mut work, request(block.clone(), parent));
            probe(
                &mut work,
                &clock,
                &mut engine,
                (block.digest(), PayloadStatusEnum::Syncing),
            )
            .await
            .unwrap();
            work.reconcile((Height::zero(), Digest::ZERO));
            assert!(
                receiver.await.is_err(),
                "unavailable ancestry closes execution work without an invalid verdict"
            );
            assert!(engine.rx.try_recv().is_err());
        });
    }
}

#[test]
fn syncing_at_the_finalized_floor_retries_after_the_existing_delay() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let block = linked(1, B256::ZERO, 10);
        let (receiver, _) = enqueue(&mut work, request(block.clone(), None));
        probe(
            &mut work,
            &clock,
            &mut engine,
            (block.digest(), PayloadStatusEnum::Syncing),
        )
        .await
        .unwrap();
        assert!(work.next_event().now_or_never().is_none());
        clock
            .sleep(crate::application::handler::VERIFY_SYNCING_RETRY_DELAY)
            .await;
        let event = work.next_event().await;
        work.handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .unwrap();
        probe(
            &mut work,
            &clock,
            &mut engine,
            (block.digest(), PayloadStatusEnum::Valid),
        )
        .await
        .unwrap();
        assert_eq!(receiver.await.unwrap(), VerificationOutcome::Valid);
    });
}

#[test]
fn cancellation_does_not_hide_a_fatal_in_flight_engine_failure() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let (receiver, budget) = enqueue(&mut work, request(linked(1, B256::ZERO, 11), None));
        work.schedule(&engine.handle);
        assert!(work.next_event().now_or_never().is_none());
        let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
            panic!("expected new_payload");
        };
        drop(receiver);
        let event = work.next_event().await;
        work.handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .unwrap();
        work.reconcile((Height::zero(), Digest::ZERO));
        assert!(budget.is_cancelled());
        drop(tx);
        let event = work.next_event().await;
        assert!(work
            .handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .is_err());
    });
}

#[test]
fn replaced_requests_cannot_adopt_an_old_execution_reply() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let (old, budget) = enqueue(&mut work, request(linked(1, B256::ZERO, 12), None));
        work.schedule(&engine.handle);
        assert!(work.next_event().now_or_never().is_none());
        let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
            panic!("expected new_payload");
        };
        let block = linked(1, B256::ZERO, 13);
        let (mut current, _) = enqueue(&mut work, request(block.clone(), None));
        assert!(old.await.is_err());
        assert!(budget.is_cancelled());
        tx.send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Valid)))
            .unwrap();
        let event = work.next_event().await;
        work.handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .unwrap();
        assert!(current.try_recv().unwrap().is_none());
        probe(
            &mut work,
            &clock,
            &mut engine,
            (block.digest(), PayloadStatusEnum::Valid),
        )
        .await
        .unwrap();
        assert_eq!(current.await.unwrap(), VerificationOutcome::Valid);
    });
}

#[test]
fn finality_and_epoch_changes_retire_pending_work_without_a_false_verdict() {
    deterministic::Runner::default().start(|clock| async move {
        let mut work = VerificationWork::default();
        let verification = request(linked(1, B256::ZERO, 14), None);
        let fence = verification.epoch_fence.clone();
        let (receiver, budget) = enqueue(&mut work, verification);
        fence.advance_epoch(Epoch::new(1));
        work.reconcile((Height::zero(), Digest::ZERO));
        assert!(receiver.await.is_err());
        assert!(budget.is_cancelled());
        let (receiver, budget) = enqueue(&mut work, request(linked(1, B256::ZERO, 15), None));
        work.finalized(
            Round::new(Epoch::new(0), View::new(4)),
            Height::new(1),
            Digest::ZERO,
        );
        assert!(receiver.await.is_err());
        assert!(budget.is_cancelled());
        clock.sleep(std::time::Duration::from_millis(1)).await;
    });
}

#[test]
fn consensus_parent_convergence_waits_for_projection_and_survives_cancellation() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let parent = linked(1, B256::ZERO, 16);
        let required = ProjectionCheckpoint {
            block_number: 1,
            block_hash: parent.block_hash(),
        };
        let (publisher, readiness) = projection_readiness(
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: B256::ZERO,
            },
            ProjectionStatus::CatchingUp { checkpoint: None },
        );
        let pending = PendingParent {
            round: Round::new(Epoch::new(0), View::new(4)),
            digest: parent.digest(),
            height: Height::new(1),
            block: Some(parent.clone()),
            epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
        };
        assert_eq!(
            work.record_parent(pending, (Height::zero(), Digest::ZERO), readiness),
            Some((Height::zero(), Digest::ZERO))
        );
        assert!(work.next_event().now_or_never().is_none());
        let (receiver, budget) = enqueue(
            &mut work,
            request(linked(2, parent.block_hash(), 17), Some(parent.clone())),
        );
        drop(receiver);
        work.reconcile((Height::zero(), Digest::ZERO));
        assert!(budget.is_cancelled());
        publisher.publish(ProjectionStatus::Ready {
            checkpoint: required,
        });
        let event = work.next_event().await;
        work.handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .unwrap();
        assert_eq!(
            probe(
                &mut work,
                &clock,
                &mut engine,
                (parent.digest(), PayloadStatusEnum::Valid)
            )
            .await
            .unwrap(),
            None
        );
        assert_eq!(
            probe(
                &mut work,
                &clock,
                &mut engine,
                (parent.digest(), PayloadStatusEnum::Valid)
            )
            .await
            .unwrap(),
            Some((Height::new(1), parent.digest()))
        );
    });
}

#[test]
fn finalized_round_ignores_delayed_consensus_parent_messages() {
    let parent = linked(2, B256::ZERO, 18);
    let round = Round::new(Epoch::new(0), View::new(4));
    let mut work = VerificationWork::default();
    work.finalized(round, Height::new(1), Digest::ZERO);
    let (_publisher, readiness) = ready_projection(
        B256::ZERO,
        ProjectionCheckpoint {
            block_number: 2,
            block_hash: parent.block_hash(),
        },
    );
    for view in [3, 4] {
        assert_eq!(
            work.record_parent(
                PendingParent {
                    round: Round::new(Epoch::new(0), View::new(view)),
                    digest: parent.digest(),
                    height: Height::new(2),
                    block: Some(parent.clone()),
                    epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
                },
                (Height::new(1), Digest::ZERO),
                readiness.clone()
            ),
            None
        );
    }
    assert!(work.next_event().now_or_never().is_none());
}

#[test]
fn parent_convergence_propagates_fatal_projection_failure() {
    deterministic::Runner::default().start(|clock| async move {
        let parent = linked(1, B256::ZERO, 19);
        let (_publisher, readiness) = projection_readiness(
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: B256::ZERO,
            },
            ProjectionStatus::Fatal {
                checkpoint: None,
                error: ProjectionFailure::new(
                    ProjectionFailureClass::StorageBackend,
                    "failed projection",
                ),
            },
        );
        let mut work = VerificationWork::default();
        work.record_parent(
            PendingParent {
                round: Round::new(Epoch::new(0), View::new(4)),
                digest: parent.digest(),
                height: Height::new(1),
                block: Some(parent),
                epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
            },
            (Height::zero(), Digest::ZERO),
            readiness,
        );
        let event = work.next_event().await;
        let error = work
            .handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .unwrap_err();
        assert!(error.to_string().contains("failed projection"));
    });
}

#[test]
fn consensus_parent_head_commits_only_after_valid_forkchoice() {
    for status in [
        PayloadStatusEnum::Valid,
        PayloadStatusEnum::Syncing,
        PayloadStatusEnum::Invalid {
            validation_error: "bad forkchoice".into(),
        },
        PayloadStatusEnum::Accepted,
    ] {
        deterministic::Runner::default().start(|clock| async move {
            let genesis = B256::ZERO;
            let mut engine = EngineStub::new();
            let (_publisher, readiness) = ready_projection(
                genesis,
                ProjectionCheckpoint {
                    block_number: 0,
                    block_hash: genesis,
                },
            );
            let (mut actor, _mailbox) = ExecutorActor::new(
                clock.child("head"),
                engine.handle,
                crate::executor::actor::RecoveredFinalizedState {
                    genesis_hash: genesis,
                    last_finalized_height: 0,
                    last_finalized_hash: genesis,
                },
                readiness,
                None,
            );
            let target = Digest(B256::repeat_byte(0x42));
            let valid = matches!(status, PayloadStatusEnum::Valid);
            let reply = async {
                let BeaconEngineMessage::ForkchoiceUpdated {
                    state,
                    payload_attrs,
                    tx,
                } = engine.rx.recv().await.unwrap()
                else {
                    panic!("expected forkchoice");
                };
                assert_eq!(state.head_block_hash, target.0);
                assert_eq!(state.finalized_block_hash, genesis);
                assert!(payload_attrs.is_none());
                tx.send(Ok(OnForkChoiceUpdated::valid(PayloadStatus::from_status(
                    status,
                ))))
                .unwrap();
            };
            let (result, ()) =
                futures::join!(actor.commit_convergence(Height::new(1), target), reply);
            assert_eq!(result.is_ok(), valid);
            assert_eq!(
                actor.state.head_height,
                if valid {
                    Height::new(1)
                } else {
                    Height::zero()
                }
            );
            assert_eq!(
                actor.state.forkchoice.head_block_hash,
                if valid { target.0 } else { genesis }
            );
        });
    }
}

#[test]
fn verification_projection_failure_reaches_executor_supervision() {
    deterministic::Runner::default().start(|clock| async move {
        let engine = EngineStub::new();
        let (_publisher, readiness) = ready_projection(
            B256::ZERO,
            ProjectionCheckpoint {
                block_number: 0,
                block_hash: B256::ZERO,
            },
        );
        let (mut actor, mailbox) = ExecutorActor::new(
            clock.child("failure"),
            engine.handle,
            crate::executor::actor::RecoveredFinalizedState {
                genesis_hash: B256::ZERO,
                last_finalized_height: 0,
                last_finalized_hash: B256::ZERO,
            },
            readiness,
            None,
        );
        mailbox
            .projection_failed(ProjectionFailure::new(
                ProjectionFailureClass::StorageBackend,
                "fatal verification projection",
            ))
            .unwrap();
        let error = actor.run_live_loop().await.unwrap_err();
        assert!(error.to_string().contains("fatal verification projection"));
    });
}
