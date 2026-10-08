//! `FinalizationActor` mailbox + message types.
//!
//! Finalization notifications use a nonblocking `UnboundedSender<Message>`.
//! Best-effort CN writes reserve bounded capacity before using that channel.
//! The voter-side reporter never awaits capacity. A closed receiver is a fatal
//! supervisor event. The mailbox reports it through the
//! [`FinalizationMailboxClosed`] error.

use crate::digest::Digest;
use crate::finalization::parent_cert_store::CertifiedParentProofRecord;
use alloy_primitives::B256;
use commonware_consensus::types::Round;
use futures::channel::mpsc;
use outbe_primitives::consensus::ConsensusData;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Returned by [`Mailbox::notify_finalized`] when the
/// `FinalizationActor` has exited and its receiver has been dropped.
/// Caller MUST log + increment a metric on this error. A silently
/// dropped finalization breaks settlement liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizationMailboxClosed;

impl core::fmt::Display for FinalizationMailboxClosed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FinalizationActor mailbox closed; finalization dropped")
    }
}

impl std::error::Error for FinalizationMailboxClosed {}

/// Messages accepted by the `FinalizationActor`.
///
/// `Finalized` carries a finalization notification. `CertifiedNotarization`
/// carries a pre-built certified-parent witness record for off-thread
/// durable persistence. The certified-notarization write goes through this
/// actor for two reasons:
/// (a) It moves the synchronous MDBX commit off the Simplex voter task.
/// (b) It keeps the actor the single durable writer to `FinalizedParentCertStore`
/// (the reporter previously wrote it inline on the voter thread).
/// The reporter builds the parity-critical record (including `committee_set_hash`)
/// before enqueue. The actor persists those verified bytes; it does not rebuild
/// the certificate or modify its protocol binding.
pub enum Message {
    Finalized(Finalized),
    CertifiedNotarization(PendingCertification),
}

/// A queued CN write owns capacity until it is persisted or dropped. Finalized
/// notifications have their existing mandatory delivery contract; only the
/// best-effort certification fallback uses this nonblocking bounded admission.
pub struct PendingCertification {
    pub record: CertifiedParentProofRecord,
    _capacity: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificationMailboxError {
    Closed,
    Full,
}
impl core::fmt::Display for CertificationMailboxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Closed => "certification mailbox closed",
            Self::Full => "certification persistence backlog full",
        })
    }
}
impl std::error::Error for CertificationMailboxError {}

/// Finalization notification routed from the consensus voter (via
/// `OutbeReporter`) into the FinalizationActor. The actor is the production
/// consumer for exact-parent cert persistence, forkchoice/status publication,
/// and finalized block-cache eviction.
pub struct Finalized {
    /// The consensus round of the finalized block.
    pub round: Round,
    /// The digest (block hash) of the finalized block.
    pub digest: Digest,
    /// VRF seed derived from the BLS threshold signature (if available).
    pub vrf_seed: Option<B256>,
    /// Full consensus data used by the actor to persist parent-cert facts and publish status.
    pub consensus_data: ConsensusData,
}

/// Handle for sending finalization events to the actor.
///
/// `notify_finalized` returns immediately via `unbounded_send`. Voter
/// task cannot block on this edge.
#[derive(Clone)]
pub struct Mailbox {
    inner: mpsc::UnboundedSender<Message>,
    certification_capacity: Arc<Semaphore>,
}

impl Mailbox {
    pub fn from_sender(tx: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            inner: tx,
            certification_capacity: Arc::new(Semaphore::new(
                crate::finalization::parent_cert_store::MAX_PENDING_CERTIFICATION_WITNESSES,
            )),
        }
    }

    /// Returns `Err(FinalizationMailboxClosed)` if the actor has exited.
    /// Caller (see `OutbeReporter::handle_finalization`) MUST log and
    /// increment the `outbe_finalization_dropped_total{reason="mailbox_closed"}`
    /// metric on Err.
    pub fn notify_finalized(&self, f: Finalized) -> Result<(), FinalizationMailboxClosed> {
        self.inner
            .unbounded_send(Message::Finalized(f))
            .map_err(|_| FinalizationMailboxClosed)
    }

    /// Enqueue a pre-built certified-parent witness record for off-thread
    /// durable persistence. Capacity is reserved without waiting, then the
    /// message is sent. `Full` drops this best-effort fallback write; `Closed`
    /// identifies actor shutdown. The reporter logs and meters either outcome.
    pub fn persist_certified_notarization(
        &self,
        record: CertifiedParentProofRecord,
    ) -> Result<(), CertificationMailboxError> {
        if self.inner.is_closed() {
            return Err(CertificationMailboxError::Closed);
        }
        let capacity = self
            .certification_capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| CertificationMailboxError::Full)?;
        self.inner
            .unbounded_send(Message::CertifiedNotarization(PendingCertification {
                record,
                _capacity: capacity,
            }))
            .map_err(|_| CertificationMailboxError::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::Digest;
    use alloy_primitives::B256;
    use commonware_consensus::types::{Epoch, Round, View};
    use futures::StreamExt;
    use outbe_primitives::consensus::ConsensusData;

    fn dummy_finalized() -> Finalized {
        Finalized {
            round: Round::new(Epoch::new(1), View::new(1)),
            digest: Digest(B256::with_last_byte(0xAA)),
            vrf_seed: Some(B256::with_last_byte(0xBB)),
            consensus_data: ConsensusData::default(),
        }
    }

    #[tokio::test]
    async fn notify_finalized_delivers_to_receiver() {
        let (tx, mut rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);

        mailbox.notify_finalized(dummy_finalized()).unwrap();

        let received = rx.next().await.expect("message delivered");
        match received {
            Message::Finalized(f) => {
                assert_eq!(f.digest.0, B256::with_last_byte(0xAA));
            }
            Message::CertifiedNotarization(_) => panic!("expected Finalized"),
        }
    }

    // The same mailbox routes certified-notarization persistence off-thread
    // as a distinct message variant.
    #[tokio::test]
    async fn persist_certified_notarization_delivers_record() {
        let (tx, mut rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);

        mailbox
            .persist_certified_notarization(CertifiedParentProofRecord::default())
            .unwrap();

        let received = rx.next().await.expect("message delivered");
        assert!(matches!(received, Message::CertifiedNotarization(_)));
    }

    #[tokio::test]
    async fn persist_certified_notarization_returns_err_on_closed_receiver() {
        let (tx, rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);
        drop(rx);

        let err = mailbox
            .persist_certified_notarization(CertifiedParentProofRecord::default())
            .unwrap_err();
        assert_eq!(err, CertificationMailboxError::Closed);
    }

    #[test]
    fn stalled_certification_writer_has_bounded_backlog_and_recovers_capacity() {
        let (tx, mut rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);
        for _ in 0..crate::finalization::parent_cert_store::MAX_PENDING_CERTIFICATION_WITNESSES {
            mailbox
                .persist_certified_notarization(CertifiedParentProofRecord::default())
                .unwrap();
        }
        assert!(
            mailbox
                .persist_certified_notarization(CertifiedParentProofRecord::default())
                .is_err(),
            "stalled persistence must not admit an unbounded CN backlog"
        );
        drop(rx.try_recv().unwrap());
        mailbox
            .persist_certified_notarization(CertifiedParentProofRecord::default())
            .unwrap();
    }

    #[tokio::test]
    async fn notify_finalized_returns_err_on_closed_receiver() {
        let (tx, rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);
        drop(rx);

        let err = mailbox.notify_finalized(dummy_finalized()).unwrap_err();
        assert_eq!(err, FinalizationMailboxClosed);
    }

    #[tokio::test]
    async fn notify_finalized_is_non_blocking_for_burst() {
        // Send 10_000 messages with no concurrent receiver. unbounded_send
        // returns instantly each time. The reads happen after.
        let (tx, mut rx) = mpsc::unbounded::<Message>();
        let mailbox = Mailbox::from_sender(tx);

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            mailbox.notify_finalized(dummy_finalized()).unwrap();
        }
        let burst_elapsed = start.elapsed();
        assert!(
            burst_elapsed < std::time::Duration::from_millis(500),
            "10k unbounded_send burst took {:?} - should be sub-second",
            burst_elapsed
        );

        let mut count = 0;
        while rx.next().await.is_some() {
            count += 1;
            if count == 10_000 {
                break;
            }
        }
        assert_eq!(count, 10_000);
    }
}
