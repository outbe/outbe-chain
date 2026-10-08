use super::*;

#[test]
fn retired_execution_accepts_only_cancellation_after_finality_or_epoch_change() {
    for epoch_change in [false, true] {
        deterministic::Runner::default().start(|clock| async move {
            let mut engine = EngineStub::new();
            let mut work = VerificationWork::default();
            let block = linked(1, B256::ZERO, 35);
            let pending = request(block.clone(), None);
            let round = pending.round;
            let fence = pending.epoch_fence.clone();
            let (receiver, budget) = enqueue(&mut work, pending);
            work.schedule(&engine.handle);
            assert!(work.next_event().now_or_never().is_none());
            let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
                panic!("expected new_payload");
            };
            if epoch_change {
                fence.advance_epoch(Epoch::new(1));
                work.reconcile((Height::zero(), Digest::ZERO));
            } else {
                work.finalized(round, Height::new(1), block.digest());
            }
            assert!(budget.is_cancelled());
            assert!(receiver.await.is_err());
            tx.send(Err(
                reth_ethereum::node::api::BeaconOnNewPayloadError::internal(
                    outbe_primitives::projection::ExecutionReadCancelled { budget },
                ),
            ))
            .unwrap();
            let event = work.next_event().await;
            assert!(work
                .handle_event(event, &clock, (Height::zero(), Digest::ZERO))
                .unwrap()
                .is_none());
        });
    }
}

#[test]
fn cancelled_budget_does_not_hide_an_unrelated_internal_engine_error() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let (receiver, budget) = enqueue(&mut work, request(linked(1, B256::ZERO, 36), None));
        work.schedule(&engine.handle);
        assert!(work.next_event().now_or_never().is_none());
        let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
            panic!("expected new_payload");
        };
        budget.cancel();
        drop(receiver);
        tx.send(Err(
            reth_ethereum::node::api::BeaconOnNewPayloadError::internal(std::io::Error::other(
                "body read request deadline exceeded",
            )),
        ))
        .unwrap();
        let event = work.next_event().await;
        assert!(work
            .handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .is_err());
    });
}

#[test]
fn cancellation_requires_the_exact_cancelled_execution_budget() {
    for mismatch in [false, true] {
        deterministic::Runner::default().start(|clock| async move {
            let mut engine = EngineStub::new();
            let mut work = VerificationWork::default();
            let (_receiver, budget) = enqueue(&mut work, request(linked(1, B256::ZERO, 32), None));
            work.schedule(&engine.handle);
            assert!(work.next_event().now_or_never().is_none());
            let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
                panic!("expected new_payload");
            };
            let abort_budget = if mismatch {
                budget.cancel();
                let other = ExecutionReadBudget::new();
                other.cancel();
                other
            } else {
                budget
            };
            tx.send(Err(
                reth_ethereum::node::api::BeaconOnNewPayloadError::internal(
                    outbe_primitives::projection::ExecutionReadCancelled {
                        budget: abort_budget,
                    },
                ),
            ))
            .unwrap();
            let event = work.next_event().await;
            assert!(work
                .handle_event(event, &clock, (Height::zero(), Digest::ZERO))
                .is_err());
        });
    }
}

#[test]
fn late_cancelled_delivery_cannot_retire_a_replacement_request() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let (old, budget) = enqueue(&mut work, request(linked(1, B256::ZERO, 33), None));
        work.schedule(&engine.handle);
        assert!(work.next_event().now_or_never().is_none());
        let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
            panic!("expected new_payload");
        };
        let block = linked(1, B256::ZERO, 34);
        let (current, _) = enqueue(&mut work, request(block.clone(), None));
        assert!(old.await.is_err());
        assert!(budget.is_cancelled());
        tx.send(Err(
            reth_ethereum::node::api::BeaconOnNewPayloadError::internal(
                outbe_primitives::projection::ExecutionReadCancelled { budget },
            ),
        ))
        .unwrap();
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
        assert_eq!(current.await.unwrap(), VerificationOutcome::Valid);
    });
}

#[test]
fn cancelled_body_read_retires_verification_before_response_receiver_closes() {
    deterministic::Runner::default().start(|clock| async move {
        let mut engine = EngineStub::new();
        let mut work = VerificationWork::default();
        let block = linked(1, B256::ZERO, 31);
        let (receiver, budget) = enqueue(&mut work, request(block.clone(), None));
        work.schedule(&engine.handle);
        assert!(work.next_event().now_or_never().is_none());
        let BeaconEngineMessage::NewPayload { tx, .. } = engine.rx.try_recv().unwrap() else {
            panic!("expected new_payload");
        };
        // The application cancels the budget before dropping its response receiver.
        budget.cancel();
        tx.send(Err(
            reth_ethereum::node::api::BeaconOnNewPayloadError::internal(
                outbe_primitives::projection::ExecutionReadCancelled { budget },
            ),
        ))
        .unwrap();
        let event = work.next_event().await;
        assert!(work
            .handle_event(event, &clock, (Height::zero(), Digest::ZERO))
            .unwrap()
            .is_none());
        assert!(
            receiver.await.is_err(),
            "cancelled work has no validity verdict"
        );

        let (receiver, budget) = enqueue(&mut work, request(block.clone(), None));
        assert!(!budget.is_cancelled());
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
