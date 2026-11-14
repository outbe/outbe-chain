//! Verification and consensus-parent convergence share one execution slot.
use super::EngineHandle;
use crate::{
    application::epoch_boundary::ApplicationEpochFence,
    block::ConsensusBlock,
    digest::Digest,
    executor::ingress::{PendingParent, VerificationOutcome, VerificationRequest, VerifyBlock},
    marshal_types::MarshalMailbox,
};
use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
use commonware_consensus::types::{Height, Round};
use commonware_runtime::Clock;
use futures::{future::BoxFuture, FutureExt};
use outbe_primitives::{
    projection::{ExecutionReadBudget, ExecutionReadCancelled, NodeLocalExecutionAbort},
    OutbeExecutionData,
};
use std::{collections::BTreeMap, sync::Arc, task::Poll};

mod convergence;
mod delivery;
mod walk;
use walk::{Step, Walk};

struct Verification {
    walk: Walk,
    response: futures::channel::oneshot::Sender<VerificationOutcome>,
}

#[derive(Clone, Copy)]
enum Owner {
    Verification(Round),
    Convergence,
}

pub(super) struct Delivery {
    request: ExecutionRequest,
    status: eyre::Result<ExecutionStatus>,
}

struct ExecutionRequest {
    owner: Owner,
    id: u64,
    digest: Digest,
    budget: ExecutionReadBudget,
}

enum ExecutionStatus {
    Completed(PayloadStatus),
    Cancelled(ExecutionReadCancelled),
    NodeLocalAbort(NodeLocalExecutionAbort),
}

pub(super) enum Event {
    Delivered(Delivery),
    Changed,
    Failed(eyre::Report),
}

#[derive(Default)]
pub(super) struct VerificationWork {
    queued: BTreeMap<Round, Verification>,
    convergence: Option<Walk>,
    convergence_target_height: Option<Height>,
    observed_finalized_height: Height,
    pending_round: Option<Round>,
    next_id: u64,
    execution: Option<BoxFuture<'static, Delivery>>,
    pub(super) marshal: Option<MarshalMailbox>,
    finalized_round: Option<Round>,
    network_finalized: Option<(Height, Digest)>,
}

impl VerificationWork {
    fn allocate_id(&mut self) -> u64 {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("execution request id exhausted");
        self.next_id
    }

    pub(super) fn queue(&mut self, request: VerifyBlock) {
        let VerifyBlock { request, response } = request;
        if self
            .finalized_round
            .is_some_and(|round| request.round <= round)
        {
            return;
        }
        let round = request.round;
        let id = self.allocate_id();
        if let Some(replaced) = self.queued.insert(
            round,
            Verification {
                walk: Walk::new(id, request),
                response,
            },
        ) {
            replaced.walk.budget.cancel();
        }
    }

    pub(super) fn record_parent(
        &mut self,
        parent: PendingParent,
        finalized: (Height, Digest),
    ) -> Option<(Height, Digest)> {
        if !self.accepts_parent(&parent) {
            return None;
        }
        if parent
            .block
            .as_ref()
            .filter(|b| b.digest() == parent.digest && b.number() == parent.height.get())
            .is_some()
        {
            self.convergence_target_height = Some(parent.height);
        }
        let previous_round = self.pending_round;
        self.pending_round = Some(parent.round);
        if self
            .convergence
            .as_ref()
            .is_some_and(|walk| walk.follows(&parent, previous_round))
        {
            return None;
        }
        self.convergence = None;
        if parent.height <= finalized.0 {
            return Some(finalized);
        }
        let block = parent.block.filter(|block| {
            block.digest() == parent.digest && block.number() == parent.height.get()
        })?;
        let id = self.allocate_id();
        let walk = Walk::new(
            id,
            VerificationRequest {
                round: parent.round,
                block,
                parent: None,
                epoch_fence: parent.epoch_fence,
                execution_read_budget: ExecutionReadBudget::new(),
            },
        );
        self.convergence = Some(walk);
        // Until the new parent has a VALID delivery and proven ancestry, HEAD
        // remains at the delivered finalized block rather than a prior branch.
        Some(finalized)
    }

    fn accepts_parent(&self, parent: &PendingParent) -> bool {
        self.finalized_round
            .is_none_or(|round| parent.round > round)
            && self.pending_round.is_none_or(|round| parent.round >= round)
            && parent
                .epoch_fence
                .check(parent.round, parent.height.get().saturating_add(1))
                .is_ok()
    }

    pub(super) fn finalized(&mut self, round: Round, height: Height, digest: Digest) {
        if self
            .finalized_round
            .is_some_and(|previous| round <= previous)
        {
            return;
        }
        self.finalized_round = Some(round);
        self.network_finalized = Some((height, digest));
        self.observe_finalized_height(height);
        self.queued.retain(|candidate, request| {
            if *candidate <= round {
                request.walk.budget.cancel();
                false
            } else {
                true
            }
        });
    }

    pub(super) fn observe_finalized_height(&mut self, height: Height) {
        self.observed_finalized_height = self.observed_finalized_height.max(height);
    }

    pub(super) fn record_progress(&self, head_height: Height, finalized_height: Height) {
        use crate::metrics::executor::{
            record_executor_convergence_depth_blocks, record_executor_finalization_lag_blocks,
        };
        record_executor_finalization_lag_blocks(
            self.observed_finalized_height
                .get()
                .saturating_sub(finalized_height.get()),
        );
        if let Some(target) = self.convergence_target_height {
            // Block heights fit comfortably within i64::MAX; this signed delta is metric-only.
            record_executor_convergence_depth_blocks(
                target.get() as i64 - head_height.get() as i64,
            );
        }
    }

    pub(super) fn reconcile(&mut self, finalized: (Height, Digest)) {
        let finalized = self.finalized_boundary(finalized);
        self.queued.retain(|_, request| {
            let keep = request.active(finalized);
            if !keep {
                request.walk.budget.cancel();
            }
            keep
        });
        if let Some(walk) = &mut self.convergence {
            if !walk.current() || walk.conflicts_with(finalized) {
                self.convergence = None;
            } else if walk.anchored.is_some_and(|digest| digest != finalized.1) {
                walk.anchored = None;
                walk.reprobe();
            }
        }
    }

    fn finalized_boundary(&self, delivered: (Height, Digest)) -> (Height, Digest) {
        self.network_finalized
            .filter(|(height, _)| *height >= delivered.0)
            .unwrap_or(delivered)
    }

    pub(super) fn schedule(&mut self, engine: &EngineHandle) {
        if self.execution.is_some() {
            return;
        }
        let selected = self
            .queued
            .iter_mut()
            .rev()
            .find_map(|(round, request)| {
                matches!(request.walk.step, Step::Ready)
                    .then_some((Owner::Verification(*round), &mut request.walk))
            })
            .or_else(|| {
                self.convergence.as_mut().and_then(|walk| {
                    matches!(walk.step, Step::Ready).then_some((Owner::Convergence, walk))
                })
            });
        let Some((owner, walk)) = selected else {
            return;
        };
        let id = walk.id;
        let digest = walk.cursor.digest();
        let height = Height::new(walk.cursor.number());
        let execution_data =
            OutbeExecutionData::new(Arc::new(walk.cursor.as_ref().clone().into_inner()))
                .with_execution_read_budget(walk.budget.clone());
        let engine = engine.clone();
        let budget = walk.budget.clone();
        walk.step = Step::InFlight;
        self.execution = Some(
            async move {
                let status = if crate::test_faults::should_drop_new_payload_for_test(height) {
                    Ok(ExecutionStatus::Completed(PayloadStatus::from_status(
                        PayloadStatusEnum::Valid,
                    )))
                } else {
                    let started = std::time::Instant::now();
                    let result = engine.new_payload(execution_data).await;
                    if matches!(owner, Owner::Verification(_)) {
                        crate::metrics::executor::record_block_verify_execution_ms(
                            started.elapsed(),
                        );
                    }
                    ExecutionStatus::from_engine(result, digest)
                };
                Delivery {
                    request: ExecutionRequest {
                        owner,
                        id,
                        digest,
                        budget,
                    },
                    status,
                }
            }
            .boxed(),
        );
    }

    fn poll_execution(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Event> {
        let Some(execution) = &mut self.execution else {
            return Poll::Pending;
        };
        let delivery = futures::ready!(execution.as_mut().poll(cx));
        self.execution = None;
        Poll::Ready(Event::Delivered(delivery))
    }

    pub(super) async fn next_event(&mut self) -> Event {
        futures::future::poll_fn(|cx| {
            if let Poll::Ready(event) = self.poll_execution(cx) {
                return Poll::Ready(event);
            }
            for request in self.queued.values_mut() {
                if let Poll::Ready(event) = request.poll(cx) {
                    return Poll::Ready(event);
                }
            }
            let Some(walk) = &mut self.convergence else {
                return Poll::Pending;
            };
            walk.poll(cx).map(Event::from_progress)
        })
        .await
    }
}

impl Verification {
    fn active(&self, finalized: (Height, Digest)) -> bool {
        if self.walk.budget.is_cancelled() || self.response.is_canceled() {
            return false;
        }
        self.walk.current()
            && !self.walk.conflicts_with(finalized)
            && !matches!(self.walk.step, Step::Stopped)
    }

    fn poll(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Event> {
        if self.response.poll_canceled(cx).is_ready() {
            return Poll::Ready(Event::Changed);
        }
        self.walk.poll(cx).map(Event::from_progress)
    }
}

impl Event {
    fn from_progress(result: eyre::Result<()>) -> Self {
        result.map_or_else(Event::Failed, |_| Event::Changed)
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;
    use commonware_consensus::types::{Epoch, View};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::{node::api::BeaconEngineMessage, primitives::SealedBlock, Block};

    #[test]
    fn engine_call_timing_is_emitted_only_for_verification_owner() {
        let round = Round::new(Epoch::new(0), View::new(4));
        for owner in [Owner::Verification(round), Owner::Convergence] {
            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                futures::executor::block_on(async {
                    let mut block = Block::default();
                    block.header.number = 55;
                    let block = Arc::new(ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                        block.map_header(OutbeHeader::new),
                    )));
                    let mut work = VerificationWork::default();
                    let (response, _receiver) = futures::channel::oneshot::channel();
                    match owner {
                        Owner::Verification(_) => work.queue(VerifyBlock {
                            request: VerificationRequest {
                                round,
                                block,
                                parent: None,
                                epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
                                execution_read_budget: ExecutionReadBudget::new(),
                            },
                            response,
                        }),
                        Owner::Convergence => {
                            work.record_parent(
                                PendingParent {
                                    round,
                                    height: Height::new(block.number()),
                                    digest: block.digest(),
                                    block: Some(block),
                                    epoch_fence: ApplicationEpochFence::new(Epoch::new(0)),
                                },
                                (Height::zero(), Digest::ZERO),
                            );
                        }
                    }
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    work.schedule(&EngineHandle::new(tx));
                    let reply = async {
                        let Some(BeaconEngineMessage::NewPayload { tx, .. }) = rx.recv().await
                        else {
                            panic!("expected engine new_payload call");
                        };
                        tx.send(Ok(PayloadStatus::from_status(PayloadStatusEnum::Valid)))
                            .expect("execution receiver remains alive");
                    };
                    let (event, ()) = futures::join!(work.next_event(), reply);
                    let Event::Delivered(delivery) = event else {
                        panic!("expected engine delivery");
                    };
                    assert_eq!(
                        matches!(delivery.request.owner, Owner::Verification(_)),
                        matches!(owner, Owner::Verification(_))
                    );
                    assert!(
                        matches!(delivery.status.unwrap(), ExecutionStatus::Completed(status) if status.is_valid())
                    );
                });
            });
            let snapshot = snapshotter.snapshot().into_vec();
            let metric = snapshot.iter().find(|(key, _, _, _)| {
                key.key().name() == "outbe_consensus_block_verify_execution_ms"
            });
            match owner {
                Owner::Verification(_) => {
                    let (_, _, _, value) = metric.expect("verification engine call is timed");
                    assert!(matches!(value, DebugValue::Histogram(samples) if samples.len() == 1));
                }
                Owner::Convergence => assert!(metric.is_none(), "convergence is not verification"),
            }
        }
    }
}
