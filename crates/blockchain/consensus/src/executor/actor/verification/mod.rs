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
    projection::{ExecutionReadBudget, ExecutionReadCancelled},
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
        readiness: outbe_primitives::projection::ProjectionReadinessHandle,
    ) -> Option<(Height, Digest)> {
        if !self.accepts_parent(&parent) {
            return None;
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
        let mut walk = Walk::new(
            id,
            VerificationRequest {
                round: parent.round,
                block,
                parent: None,
                epoch_fence: parent.epoch_fence,
                execution_read_budget: ExecutionReadBudget::new(),
            },
        );
        let checkpoint = outbe_primitives::projection::ProjectionCheckpoint {
            block_number: parent.height.get(),
            block_hash: parent.digest.0,
        };
        walk.step = Step::Projection(
            readiness
                .wait_for(checkpoint, std::future::pending())
                .boxed(),
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
        self.queued.retain(|candidate, request| {
            if *candidate <= round {
                request.walk.budget.cancel();
                false
            } else {
                true
            }
        });
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
                    ExecutionStatus::from_engine(engine.new_payload(execution_data).await, digest)
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

impl ExecutionStatus {
    fn from_engine(
        result: Result<PayloadStatus, reth_ethereum::node::api::BeaconOnNewPayloadError>,
        digest: Digest,
    ) -> eyre::Result<Self> {
        let error = match result {
            Ok(status) => return Ok(Self::Completed(status)),
            Err(error) => error,
        };
        let cancelled = match &error {
            reth_ethereum::node::api::BeaconOnNewPayloadError::Internal(source) => {
                ExecutionReadCancelled::find(source.as_ref())
            }
            _ => None,
        };
        if let Some(cancelled) = cancelled {
            return Ok(Self::Cancelled(cancelled.clone()));
        }
        Err(eyre::Report::new(error).wrap_err(format!(
            "new_payload failed during executor verification: target={digest}"
        )))
    }
}
