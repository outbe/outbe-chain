use std::time::Duration;

use alloy_primitives::Bytes;
use tokio::sync::{mpsc, oneshot};

use super::ConsensusExecutionBridge;

/// Capacity of the bounded finalization request queue (256 queued + at most
/// 1 active request in the sequential drainer = 257 in-flight requests).
///
/// An unbounded queue does not protect the drain; it only converts back-pressure
/// into memory growth and latency. A full queue sheds immediately so an
/// overloaded node answers fast. Provisional node-local policy; not
/// latency-calibrated.
pub const FINALIZATION_QUEUE_DEPTH: usize = 256;

/// Maximum time a caller waits for a single finalization request (including
/// queue residence) and the total budget for a `getFinalityProof` walk.
///
/// Chosen to stay 5s below `UpstreamLimits::DEFAULT.request_timeout` (10s).
pub const FINALIZATION_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The marshal-backed answer to an `outbe_getFinalization` request. It holds
/// the commonware-codec-encoded finalization certificate and the encoded
/// `ConsensusBlock`, ready to hex and ship. A follower's resolver reconstructs
/// `finalization.encode() || block.encode()` from exactly these two fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedBlockBytes {
    /// `commonware_codec::Encode` bytes of the direct `Finalization` certificate.
    /// Empty for indirectly finalized ancestors; getFinalization must reject
    /// that case while getFinalityProof can supply an authenticated descendant.
    pub finalization: Bytes,
    /// `commonware_codec::Encode` bytes of the finalized `ConsensusBlock`.
    pub block: Bytes,
}

/// Failure of a [`ConsensusExecutionBridge::request_finalization`] call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FinalizationRequestError {
    /// No finalization fetcher is installed or the drainer channel closed.
    #[error("finalization fetcher is not installed or the queue is closed")]
    NotServing,
    /// The bounded finalization request queue is full.
    #[error("finalization request queue is full")]
    Busy,
    /// Waiting for the drainer exceeded [`FINALIZATION_REQUEST_TIMEOUT`].
    #[error("finalization request timed out")]
    TimedOut,
}

/// Sender half of the finalization fetch channel: a height plus a one-shot
/// reply. It lives in the bridge so the `outbe-rpc` handler can request
/// finalized bytes from the consensus thread. That handler cannot see the
/// marshal or `ConsensusBlock`.
pub(super) type FinalizationFetcherTx =
    mpsc::Sender<(u64, oneshot::Sender<Option<FinalizedBlockBytes>>)>;

/// Receiver half handed to the consensus-side drainer.
pub type FinalizationFetcherRx =
    mpsc::Receiver<(u64, oneshot::Sender<Option<FinalizedBlockBytes>>)>;

impl ConsensusExecutionBridge {
    /// Installs the bounded finalization fetcher channel and stores the sender.
    /// Called once at marshal-start (validator and follower paths both register
    /// one). The consensus thread drains the matching [`FinalizationFetcherRx`]
    /// and answers from the marshal. Returns the receiver to hand to the drainer.
    pub fn set_finalization_fetcher(&self) -> FinalizationFetcherRx {
        let (tx, rx) = mpsc::channel(FINALIZATION_QUEUE_DEPTH);
        self.lock_state().finalization_fetcher = Some(tx);
        rx
    }

    /// Requests the finalized certificate + block bytes for `height` from the
    /// consensus thread (used by `outbe_getFinalization`, `outbe_getConsensusBlock`,
    /// and `outbe_getFinalityProof`).
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` when the drainer found the finalized block.
    /// - `Ok(None)` when the drainer looked and the block is absent at `height`.
    /// - `Err(FinalizationRequestError::NotServing)` when no fetcher is installed,
    ///   the request channel is closed, or the reply channel was dropped.
    /// - `Err(FinalizationRequestError::Busy)` when the bounded queue is full.
    /// - `Err(FinalizationRequestError::TimedOut)` when waiting for the reply
    ///   exceeds [`FINALIZATION_REQUEST_TIMEOUT`].
    pub async fn request_finalization(
        &self,
        height: u64,
    ) -> Result<Option<FinalizedBlockBytes>, FinalizationRequestError> {
        let sender = {
            let state = self.lock_state();
            state
                .finalization_fetcher
                .clone()
                .ok_or(FinalizationRequestError::NotServing)?
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        match sender.try_send((height, reply_tx)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return Err(FinalizationRequestError::Busy);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(FinalizationRequestError::NotServing);
            }
        }
        match tokio::time::timeout(FINALIZATION_REQUEST_TIMEOUT, reply_rx).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(_)) => Err(FinalizationRequestError::NotServing),
            Err(_) => Err(FinalizationRequestError::TimedOut),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::consensus::{ConsensusStatus, RandomnessStatus};

    #[tokio::test]
    async fn finalization_queue_sheds_when_full() {
        let bridge = Arc::new(ConsensusExecutionBridge::new());
        let mut rx = bridge.set_finalization_fetcher();

        let mut waiters = Vec::with_capacity(FINALIZATION_QUEUE_DEPTH);
        for height in 0..FINALIZATION_QUEUE_DEPTH as u64 {
            let b = Arc::clone(&bridge);
            waiters.push(tokio::spawn(
                async move { b.request_finalization(height).await },
            ));
            tokio::task::yield_now().await;
        }
        assert_eq!(rx.len(), FINALIZATION_QUEUE_DEPTH);

        let err = bridge
            .request_finalization(FINALIZATION_QUEUE_DEPTH as u64)
            .await
            .unwrap_err();
        assert_eq!(err, FinalizationRequestError::Busy);

        // Drain one item and verify capacity is released for the next admission.
        let (first_height, first_reply) = rx.recv().await.expect("first queued item");
        assert_eq!(first_height, 0);
        let expected = FinalizedBlockBytes {
            finalization: Bytes::from_static(&[0x01]),
            block: Bytes::from_static(&[0x02]),
        };
        first_reply.send(Some(expected.clone())).unwrap();
        assert_eq!(waiters.remove(0).await.unwrap(), Ok(Some(expected)));

        let readmitted = Arc::clone(&bridge);
        let admitted_handle =
            tokio::spawn(async move { readmitted.request_finalization(999).await });
        tokio::task::yield_now().await;
        assert_eq!(rx.len(), FINALIZATION_QUEUE_DEPTH);

        for waiter in waiters {
            waiter.abort();
        }
        admitted_handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn finalization_request_times_out() {
        let bridge = Arc::new(ConsensusExecutionBridge::new());
        let mut rx = bridge.set_finalization_fetcher();

        let b = Arc::clone(&bridge);
        let handle = tokio::spawn(async move { b.request_finalization(42).await });
        tokio::task::yield_now().await;

        let (height, _held_reply_tx) = rx.recv().await.expect("queued request");
        assert_eq!(height, 42);

        tokio::time::advance(FINALIZATION_REQUEST_TIMEOUT).await;
        let res = handle.await.unwrap();
        assert_eq!(res, Err(FinalizationRequestError::TimedOut));
    }

    #[tokio::test]
    async fn finalization_request_lifecycle_states() {
        // 1. No fetcher installed -> NotServing.
        let bridge = ConsensusExecutionBridge::new();
        assert_eq!(
            bridge.request_finalization(1).await,
            Err(FinalizationRequestError::NotServing)
        );

        // 2. Receiver dropped before admission -> NotServing.
        let rx = bridge.set_finalization_fetcher();
        drop(rx);
        assert_eq!(
            bridge.request_finalization(1).await,
            Err(FinalizationRequestError::NotServing)
        );

        // 3. Reply sender dropped after admission -> NotServing.
        let mut rx = bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            if let Some((_h, reply_tx)) = rx.recv().await {
                drop(reply_tx);
            }
        });
        assert_eq!(
            bridge.request_finalization(2).await,
            Err(FinalizationRequestError::NotServing)
        );

        // 4. Drainer replies None -> Ok(None); Some(bytes) -> Ok(Some(bytes)).
        let mut rx = bridge.set_finalization_fetcher();
        let expected = FinalizedBlockBytes {
            finalization: Bytes::from_static(&[0xaa]),
            block: Bytes::from_static(&[0xbb]),
        };
        let expected_clone = expected.clone();
        tokio::spawn(async move {
            if let Some((h, reply_tx)) = rx.recv().await {
                assert_eq!(h, 3);
                let _ = reply_tx.send(None);
            }
            if let Some((h, reply_tx)) = rx.recv().await {
                assert_eq!(h, 4);
                let _ = reply_tx.send(Some(expected_clone));
            }
        });
        assert_eq!(bridge.request_finalization(3).await, Ok(None));
        assert_eq!(bridge.request_finalization(4).await, Ok(Some(expected)));
    }

    #[test]
    fn update_consensus_status_preserves_last_finalized_block() {
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_consensus_status(ConsensusStatus {
            current_view: 50,
            last_finalized_block: 48,
            ..Default::default()
        });

        bridge.update_consensus_status(|status| {
            status.randomness_status = RandomnessStatus::Healthy;
            status.vrf_material_version = 3;
            status.last_dkg_activation_height = 10;
            status.next_planned_activation_height = 110;
            status.vrf_expiry_height = 210;
        });

        let snapshot = bridge.consensus_status();
        assert_eq!(snapshot.last_finalized_block, 48);
        assert_eq!(snapshot.current_view, 50);
        assert_eq!(snapshot.randomness_status, RandomnessStatus::Healthy);
        assert_eq!(snapshot.vrf_material_version, 3);
        assert_eq!(snapshot.last_dkg_activation_height, 10);
        assert_eq!(snapshot.next_planned_activation_height, 110);
        assert_eq!(snapshot.vrf_expiry_height, 210);
    }
}
