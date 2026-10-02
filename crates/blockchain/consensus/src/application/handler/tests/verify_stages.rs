use super::*;
use futures::{FutureExt as _, StreamExt as _};
use outbe_primitives::projection::ExecutionReadBudget;
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::system_tx::SystemTxInputV2;
use reth_ethereum::node::api::BeaconEngineMessage;

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
enum BlockCase {
    Valid,
    Invalid,
    SyncingThenValid,
    Cancelled,
    StaleEpoch,
}

#[derive(Clone, Copy, Debug)]
enum ParentCase {
    None,
    Valid,
    Invalid,
    SyncingThenValid,
}

#[test]
fn verify_execution_preserves_verdict_and_side_effect_order() {
    for (parent_case, block_case) in [
        (ParentCase::None, BlockCase::Valid),
        (ParentCase::None, BlockCase::Invalid),
        (ParentCase::None, BlockCase::SyncingThenValid),
        (ParentCase::None, BlockCase::Cancelled),
        (ParentCase::None, BlockCase::StaleEpoch),
        (ParentCase::Valid, BlockCase::Valid),
        (ParentCase::Valid, BlockCase::Invalid),
        (ParentCase::Invalid, BlockCase::Valid),
        (ParentCase::SyncingThenValid, BlockCase::Valid),
    ] {
        commonware_runtime::deterministic::Runner::timed(Duration::from_secs(30)).start(
            |context| async move {
                let clock = context.child("verify_matrix");
                let (marshal, keepalive, actor) =
                    start_marshal_without_available_block(context).await;
                let mut shared = finalizer_test_shared(marshal, HybridSchemeProvider::new());
                let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
                shared.shared.engine = super::super::EngineHandle::new(engine_tx);
                let (executor_tx, mut executor_rx) = futures::channel::mpsc::unbounded();
                shared.shared.executor_mailbox = crate::executor::Mailbox::from_sender(executor_tx);

                let parent = match parent_case {
                    ParentCase::None => None,
                    _ => Some(consensus_block_with_timestamp(0x71, 1, 2_000)),
                };
                let parent_digest = parent
                    .as_ref()
                    .map_or(Digest(B256::ZERO), ConsensusBlock::digest);
                if let Some(parent) = &parent {
                    shared
                        .block_cache
                        .insert_bounded(parent_digest, parent.clone());
                    shared
                        ._projection_publisher
                        .publish(ProjectionStatus::Ready {
                            checkpoint: ProjectionCheckpoint {
                                block_number: 1,
                                block_hash: parent_digest.0,
                            },
                        });
                }
                let block = proposed_block(parent.as_ref(), false);
                shared
                    .block_cache
                    .insert_bounded(block.digest(), block.clone());
                let request = verify_context(parent_digest);
                let budget = ExecutionReadBudget::new();
                let (response, receiver) = oneshot::channel();
                let mut receiver = Some(receiver);
                let mut delivered_before_canonicalize = false;
                let verify =
                    shared.handle_verify(&clock, request, block.digest(), response, budget.clone());
                let execution = async {
                    if let Some(parent) = &parent {
                        let statuses: &[PayloadStatusEnum] = match parent_case {
                            ParentCase::Invalid => &[PayloadStatusEnum::Invalid {
                                validation_error: "parent invalid".into(),
                            }],
                            ParentCase::SyncingThenValid => {
                                &[PayloadStatusEnum::Syncing, PayloadStatusEnum::Valid]
                            }
                            _ => &[PayloadStatusEnum::Valid],
                        };
                        for status in statuses {
                            let BeaconEngineMessage::NewPayload { payload, tx } =
                                engine_rx.recv().await.unwrap()
                            else {
                                panic!("verification must call new_payload");
                            };
                            assert_eq!(
                                reth_node_builder::ExecutionPayload::block_hash(&payload),
                                parent.block_hash()
                            );
                            tx.send(Ok(PayloadStatus::from_status(status.clone())))
                                .unwrap();
                        }
                        if matches!(parent_case, ParentCase::Invalid) {
                            return;
                        }
                        if matches!(parent_case, ParentCase::Valid) {
                            let crate::executor::ingress::Message::CanonicalizeHead(head) =
                                executor_rx.next().await.unwrap()
                            else {
                                panic!("parent must be canonicalized before proposed execution");
                            };
                            assert_eq!(head.digest, parent_digest);
                            assert!(receiver.as_mut().unwrap().now_or_never().is_none());
                            assert_eq!(shared.finalization_view.timestamp_floor(), 0);
                            head.response.send(Ok(())).unwrap();
                        }
                    }
                    let statuses: &[PayloadStatusEnum] = match block_case {
                        BlockCase::SyncingThenValid => {
                            &[PayloadStatusEnum::Syncing, PayloadStatusEnum::Valid]
                        }
                        BlockCase::Invalid => &[PayloadStatusEnum::Invalid {
                            validation_error: "block invalid".into(),
                        }],
                        _ => &[PayloadStatusEnum::Valid],
                    };
                    for status in statuses {
                        let BeaconEngineMessage::NewPayload { payload, tx } =
                            engine_rx.recv().await.unwrap()
                        else {
                            panic!("verification must call new_payload");
                        };
                        assert_eq!(
                            reth_node_builder::ExecutionPayload::block_hash(&payload),
                            block.block_hash()
                        );
                        if matches!(parent_case, ParentCase::SyncingThenValid) {
                            assert!(executor_rx.next().now_or_never().is_none());
                            assert_eq!(shared.finalization_view.timestamp_floor(), 0);
                        }
                        if matches!(block_case, BlockCase::Cancelled) {
                            drop(receiver.take());
                            // Hold the engine sender until response cancellation wins the race.
                            while !budget.is_cancelled() {
                                clock.sleep(Duration::from_millis(1)).await;
                            }
                            return;
                        }
                        if matches!(block_case, BlockCase::StaleEpoch) {
                            shared.epoch_fence.advance_epoch(Epoch::new(1));
                        }
                        tx.send(Ok(PayloadStatus::from_status(status.clone())))
                            .unwrap();
                    }
                    if matches!(block_case, BlockCase::Valid) {
                        let crate::executor::ingress::Message::CanonicalizeHead(head) =
                            executor_rx.next().await.unwrap()
                        else {
                            panic!("VALID block must be canonicalized");
                        };
                        assert_eq!(head.digest, block.digest());
                        assert_eq!(receiver.as_mut().unwrap().now_or_never(), Some(Ok(true)));
                        delivered_before_canonicalize = true;
                        head.response.send(Ok(())).unwrap();
                    }
                };
                let (result, ()) = futures::join!(verify, execution);
                result.unwrap();
                if !delivered_before_canonicalize {
                    match (parent_case, block_case) {
                        (ParentCase::Invalid, _) | (_, BlockCase::Invalid) => {
                            assert!(!receiver.take().unwrap().await.unwrap())
                        }
                        (_, BlockCase::SyncingThenValid) => {
                            assert!(receiver.take().unwrap().await.unwrap())
                        }
                        (_, BlockCase::Cancelled) => assert!(budget.is_cancelled()),
                        (_, BlockCase::StaleEpoch) => {
                            assert!(receiver.take().unwrap().await.is_err())
                        }
                        _ => panic!("missing canonicalization for {parent_case:?}/{block_case:?}"),
                    }
                }
                let expected_floor = if delivered_before_canonicalize {
                    3_000
                } else if matches!(parent_case, ParentCase::Valid) {
                    2_000
                } else {
                    0
                };
                assert_eq!(
                    shared.finalization_view.timestamp_floor(),
                    expected_floor,
                    "{parent_case:?}/{block_case:?}"
                );
                assert!(executor_rx.next().now_or_never().is_none());
                assert!(engine_rx.try_recv().is_err());
                drop(keepalive);
                actor.abort();
                let _ = actor.await;
            },
        );
    }
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
                let mut shared = finalizer_test_shared(marshal, HybridSchemeProvider::new());
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
                                request,
                                block.digest(),
                                response,
                                budget.clone(),
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
                let result = shared
                    .handle_verify(&clock, request, block.digest(), response, budget)
                    .await;
                if matches!(case, EarlyCase::MissingEpochAnchor) {
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("could not resolve epoch boundary parent"));
                    assert!(receiver.take().unwrap().await.is_err());
                } else {
                    result.unwrap();
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
