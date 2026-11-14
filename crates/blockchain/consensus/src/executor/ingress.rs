//! Executor actor mailbox and message types.

use crate::digest::Digest;
use crate::metrics::executor::record_executor_queue_entries;
use alloy_rpc_types_engine::PayloadId;
use commonware_consensus::types::{Height, Round};
use commonware_utils::channel::oneshot;
use futures::{channel::mpsc, Stream, StreamExt};
use outbe_primitives::OutbePayloadAttributes;
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Instant,
};

/// Handle for sending messages to the executor actor.
///
/// sus-1: this mailbox is intentionally **unbounded**. The executor is the sole
/// recovery-critical sink for marshal-delivered finalized blocks. The consensus block rate
/// naturally bounds its inflow: one finalized block per finalized view, plus low-rate
/// canonicalize/heartbeat traffic. It is not a fan-in hot path.
///
/// Because the mailbox is unbounded, `Reporter::report` here never returns
/// `Feedback::Backoff`. An unbounded `unbounded_send` only fails when the receiver is gone,
/// and `report` surfaces that as `Feedback::Closed`. Thus nothing can silently throttle the
/// upstream `let _ = report(...)` sites.
///
/// A bounded queue with an overflow policy would let marshal/Simplex throttle under sustained
/// pressure. That change is deferred. If executor mailbox depth ever becomes a concern, add a
/// depth metric here before you switch to a bounded queue. The same holds for the
/// peer_manager and finalization endpoints.
#[derive(Clone)]
pub struct Mailbox {
    inner: MailboxInner,
}

#[derive(Clone)]
enum MailboxInner {
    Direct(mpsc::UnboundedSender<Message>),
    Instrumented {
        tx: mpsc::UnboundedSender<QueuedMessage>,
        depth: Arc<AtomicUsize>,
        publication: Arc<parking_lot::Mutex<()>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("executor mailbox closed")]
struct MailboxClosed;

struct QueueDepthGuard {
    depth: Arc<AtomicUsize>,
    publication: Arc<parking_lot::Mutex<()>>,
}

impl QueueDepthGuard {
    fn new(depth: Arc<AtomicUsize>, publication: Arc<parking_lot::Mutex<()>>) -> Self {
        {
            // Serialize gauge publication as well as the count to avoid stale concurrent sets.
            let _lock = publication.lock();
            record_executor_queue_entries(depth.fetch_add(1, Ordering::Relaxed) + 1);
        }
        Self { depth, publication }
    }
}

impl Drop for QueueDepthGuard {
    fn drop(&mut self) {
        let _lock = self.publication.lock();
        record_executor_queue_entries(self.depth.fetch_sub(1, Ordering::Relaxed) - 1);
    }
}

struct QueuedMessage {
    enqueued_at: Instant,
    message: Message,
    _depth_guard: QueueDepthGuard,
}

pub(crate) struct InstrumentedReceiver {
    inner: mpsc::UnboundedReceiver<QueuedMessage>,
}

impl Stream for InstrumentedReceiver {
    type Item = (Instant, Message);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx).map(|queued| {
            queued.map(
                |QueuedMessage {
                     enqueued_at,
                     message,
                     ..
                 }| (enqueued_at, message),
            )
        })
    }
}

impl Mailbox {
    pub(crate) fn channel() -> (Self, InstrumentedReceiver) {
        let (tx, inner) = mpsc::unbounded();
        record_executor_queue_entries(0);
        let inner_sender = MailboxInner::Instrumented {
            tx,
            depth: Arc::new(AtomicUsize::new(0)),
            publication: Arc::new(parking_lot::Mutex::new(())),
        };
        (
            Self {
                inner: inner_sender,
            },
            InstrumentedReceiver { inner },
        )
    }

    fn try_send(&self, message: Message) -> Result<(), MailboxClosed> {
        let sent = match &self.inner {
            MailboxInner::Direct(tx) => tx.unbounded_send(message).map_err(|_| ()),
            MailboxInner::Instrumented {
                tx,
                depth,
                publication,
            } => {
                let queued = QueuedMessage {
                    enqueued_at: Instant::now(),
                    message,
                    _depth_guard: QueueDepthGuard::new(depth.clone(), publication.clone()),
                };
                tx.unbounded_send(queued).map_err(|_| ())
            }
        };
        sent.map_err(|()| MailboxClosed)
    }

    pub(super) fn send(&self, message: Message) -> eyre::Result<()> {
        self.try_send(message)
            .map_err(|MailboxClosed| crate::errors::ExecutorError::MailboxClosed.into())
    }

    pub(crate) fn report_pending_parent(&self, parent: PendingParent) -> eyre::Result<()> {
        self.send(Message::PendingParent(parent))
    }

    /// Request a validity decision without selecting a canonical head.
    pub(crate) fn verify_block(
        &self,
        request: VerificationRequest,
    ) -> eyre::Result<futures::channel::oneshot::Receiver<VerificationOutcome>> {
        let (response, receiver) = futures::channel::oneshot::channel();
        self.send(Message::VerifyBlock(VerifyBlock { request, response }))?;
        Ok(receiver)
    }
    /// Create from a sender.
    pub fn from_sender(tx: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            inner: MailboxInner::Direct(tx),
        }
    }

    /// Request that the given block becomes the canonical chain head.
    ///
    /// Returns `Ok(())` after a successful FCU, or error if FCU failed/invalid.
    pub async fn canonicalize_head(&self, height: Height, digest: Digest) -> eyre::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Message::CanonicalizeHead(CanonicalizeHead {
            height,
            digest,
            response: tx,
        }))?;
        rx.await
            .map_err(|_| crate::errors::ExecutorError::DroppedResponse)?
    }

    /// Canonicalize head and request a new payload to be built.
    ///
    /// Sends FCU with payload attributes so the engine starts building a
    /// payload on the correct canonical state (with pool access).
    pub async fn canonicalize_and_build(
        &self,
        height: Height,
        digest: Digest,
        attributes: OutbePayloadAttributes,
    ) -> eyre::Result<PayloadId> {
        let (tx, rx) = oneshot::channel();
        self.send(Message::CanonicalizeAndBuild(CanonicalizeAndBuild {
            height,
            digest,
            attributes,
            response: tx,
        }))?;
        rx.await
            .map_err(|_| crate::errors::ExecutorError::DroppedResponse)?
    }

    /// Return once executor has processed/finalized at least `height`.
    ///
    /// This is a wakeup signal for secondary consumers such as peer-manager.
    /// It is not a substitute for provider-level canonical hash checks.
    pub async fn subscribe_finalized(&self, height: Height) -> eyre::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Message::SubscribeFinalized(SubscribeFinalized {
            height,
            response: tx,
        }))?;
        rx.await
            .map_err(|_| crate::errors::ExecutorError::DroppedFinalizedSubscriptionResponse.into())
    }
}

/// Implement Reporter for marshal block delivery.
///
/// Marshal calls `report(Update::Block(block, ack))` to deliver finalized blocks.
/// The executor processes the block and acknowledges back to marshal, which
/// persists the processed height - the recovery truth on restart.
impl commonware_consensus::Reporter for Mailbox {
    type Activity = crate::marshal_types::MarshalUpdate;

    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        match &activity {
            commonware_consensus::marshal::Update::Block(block, _) => {
                tracing::debug!(
                    activity_kind = "block",
                    height = block.number(),
                    digest = %block.block_hash(),
                    "marshal report received"
                );
            }
            commonware_consensus::marshal::Update::Tip(round, height, digest) => {
                tracing::debug!(
                    activity_kind = "tip",
                    %round,
                    %height,
                    %digest,
                    "marshal report received"
                );
            }
        }
        // `report` is now synchronous: enqueue the update into the executor
        // actor's mailbox and translate the channel state into `Feedback`.
        // We must not bridge to async work here (no spawn). The executor
        // drains `Message::MarshalUpdate` on its own loop and acks marshal
        // after successful EL processing. This preserves determinism.
        match self.try_send(Message::MarshalUpdate(Box::new(activity))) {
            Ok(()) => commonware_actor::Feedback::Ok,
            Err(_) => commonware_actor::Feedback::Closed,
        }
    }
}

/// Messages handled by the executor actor.
#[allow(clippy::large_enum_variant)]
pub enum Message {
    PendingParent(PendingParent),
    VerifyBlock(VerifyBlock),
    /// Request to make a block the canonical head.
    CanonicalizeHead(CanonicalizeHead),
    /// Canonicalize head and build a new payload (FCU with attributes).
    CanonicalizeAndBuild(CanonicalizeAndBuild),
    /// Finalized block delivered by marshal (with acknowledgment token).
    MarshalUpdate(Box<crate::marshal_types::MarshalUpdate>),
    /// Notify once executor finalization reaches the requested height.
    SubscribeFinalized(SubscribeFinalized),
}

/// A consensus-selected parent remains a convergence target after cancellation.
pub struct PendingParent {
    pub(crate) round: Round,
    pub(crate) digest: Digest,
    pub(crate) height: Height,
    pub(crate) block: Option<std::sync::Arc<crate::block::ConsensusBlock>>,
    pub(crate) epoch_fence: crate::application::epoch_boundary::ApplicationEpochFence,
}

pub struct VerificationRequest {
    pub(crate) round: Round,
    pub(crate) block: std::sync::Arc<crate::block::ConsensusBlock>,
    pub(crate) parent: Option<std::sync::Arc<crate::block::ConsensusBlock>>,
    pub(crate) epoch_fence: crate::application::epoch_boundary::ApplicationEpochFence,
    pub(crate) execution_read_budget: outbe_primitives::projection::ExecutionReadBudget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerificationOutcome {
    Valid,
    Invalid,
    Unavailable,
}

pub struct VerifyBlock {
    pub(crate) request: VerificationRequest,
    pub(crate) response: futures::channel::oneshot::Sender<VerificationOutcome>,
}

/// Canonicalize head request.
pub struct CanonicalizeHead {
    pub height: Height,
    pub digest: Digest,
    pub response: oneshot::Sender<eyre::Result<()>>,
}

/// Canonicalize head and build payload request.
pub struct CanonicalizeAndBuild {
    pub height: Height,
    pub digest: Digest,
    pub attributes: OutbePayloadAttributes,
    pub response: oneshot::Sender<eyre::Result<PayloadId>>,
}

/// Executor finalized-height subscription request.
pub struct SubscribeFinalized {
    pub height: Height,
    pub response: oneshot::Sender<()>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};

    fn message(height: u64) -> Message {
        let (response, _) = oneshot::channel();
        Message::SubscribeFinalized(SubscribeFinalized {
            height: Height::new(height),
            response,
        })
    }

    fn assert_depth(snapshotter: &Snapshotter, depth: f64) {
        let snapshot = snapshotter.snapshot().into_vec();
        let (_, _, _, value) = snapshot
            .iter()
            .find(|(key, _, _, _)| key.key().name() == "outbe_consensus_executor_queue_entries")
            .expect("queue depth emitted");
        assert_eq!(*value, DebugValue::Gauge(depth.into()));
    }

    #[test]
    fn queue_depth_tracks_receive_drop_and_failed_send() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            let (mailbox, mut receiver) = Mailbox::channel();
            assert_depth(&snapshotter, 0.0);
            for height in 1..=3 {
                mailbox.send(message(height)).unwrap();
            }
            assert_depth(&snapshotter, 3.0);
            let (enqueued_at, first) = receiver.next().now_or_never().flatten().unwrap();
            crate::metrics::executor::record_executor_queue_wait_ms(enqueued_at.elapsed());
            let Message::SubscribeFinalized(first) = first else {
                panic!("expected subscription")
            };
            assert_eq!(first.height, Height::new(1));
            // Snapshotter drains all values, so inspect depth and wait together.
            let snapshot = snapshotter.snapshot().into_vec();
            assert!(snapshot.iter().any(|(key, _, _, value)| {
                key.key().name() == "outbe_consensus_executor_queue_entries"
                    && *value == DebugValue::Gauge(2.0.into())
            }));
            assert!(snapshot.iter().any(|(key, _, _, value)| {
                key.key().name() == "outbe_consensus_executor_queue_wait_ms"
                    && matches!(value, DebugValue::Histogram(samples) if samples.len() == 1)
            }));
            drop(receiver);
            assert_depth(&snapshotter, 0.0);
            let error = mailbox.send(message(4)).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<crate::errors::ExecutorError>(),
                Some(crate::errors::ExecutorError::MailboxClosed)
            ));
            assert_depth(&snapshotter, 0.0);
        });
    }

    #[test]
    fn from_sender_preserves_direct_messages_and_closed_feedback() {
        let (tx, mut receiver) = mpsc::unbounded();
        let mailbox = Mailbox::from_sender(tx);
        mailbox.send(message(8)).unwrap();
        let Some(Some(Message::SubscribeFinalized(first))) = receiver.next().now_or_never() else {
            panic!("expected direct subscription");
        };
        assert_eq!(first.height, Height::new(8));
        drop(receiver);
        let error = mailbox.send(message(9)).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::errors::ExecutorError>(),
            Some(crate::errors::ExecutorError::MailboxClosed)
        ));
    }
}
