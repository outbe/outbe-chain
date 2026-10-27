//! Follower-only pre-stop barrier. Marshal must stay alive until accepted
//! execution deliveries and their exact parent proofs have drained.
//!
//! The same barrier ends the `UpstreamBootstrap` phase of a validator. The
//! follower stack then quiesces itself for promotion, and a later host drain
//! is a no-op.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use commonware_actor::Feedback;
use commonware_consensus::{marshal::Update, Reporter};
use commonware_utils::acknowledgement::Exact;
#[cfg(test)]
use eyre::eyre;
use eyre::Result;
use outbe_consensus::{executor::Mailbox, marshal_types::MarshalUpdate};
use tokio::sync::oneshot;

/// Why the follower closed its execution ingress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowerQuiesceReason {
    /// The host process stops the node.
    HostShutdown,
    /// A validator in `UpstreamBootstrap` is ready for the validator stack.
    PromoteToValidator {
        /// Local finalized height at which the node became ready.
        finalized_height: u64,
    },
}

/// How the certified follower stack ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FollowStackExit {
    /// The host stopped the node.
    Shutdown,
    /// The follower phase drained for an in-process validator start.
    PromoteToValidator {
        /// Last local finalized height of the follower phase.
        finalized_height: u64,
    },
}

#[derive(Default)]
struct Ingress {
    installed: bool,
    quiescing: bool,
    reason: Option<FollowerQuiesceReason>,
    promotion_complete: bool,
    executor: Option<Mailbox>,
    // The executor did not accept these deliveries. Do not acknowledge
    // or cancel them while Marshal is alive. Its existing max_pending_acks
    // bounds this collection. The archived blocks remain available on restart.
    withheld: Vec<Exact>,
}

/// NodeHost side of the one-shot barrier, awaited before runtime.stop.
pub struct FollowerDrainControl {
    ingress: Arc<Mutex<Ingress>>,
    completion: oneshot::Receiver<()>,
}

/// Stack side: register ingress before starting execution and acknowledge only
/// after both executor exit and finality-observer persistence succeed.
pub struct FollowerDrain {
    ingress: Arc<Mutex<Ingress>>,
    completion: oneshot::Sender<()>,
}

pub fn follower_drain_pair() -> (FollowerDrainControl, FollowerDrain) {
    let ingress = Arc::new(Mutex::new(Ingress::default()));
    let (tx, rx) = oneshot::channel();
    (
        FollowerDrainControl {
            ingress: Arc::clone(&ingress),
            completion: rx,
        },
        FollowerDrain {
            ingress,
            completion: tx,
        },
    )
}

impl FollowerDrainControl {
    pub async fn drain(self, deadline: Duration) -> Result<()> {
        let installed = {
            let mut ingress = self
                .ingress
                .lock()
                .map_err(|_| crate::errors::FollowerShutdownError::IngressLockPoisoned)?;
            if ingress.promotion_complete {
                // The follower phase already drained for promotion. The
                // validator stack owns shutdown from here.
                return Ok(());
            }
            ingress.quiescing = true;
            if ingress.reason.is_none() {
                ingress.reason = Some(FollowerQuiesceReason::HostShutdown);
            }
            // Dropping the sole sender closes ingress. The executor drains its
            // already accepted queue and closes its height notification stream.
            ingress.executor.take();
            ingress.installed
        };
        if !installed {
            // Registration races with stop under the same lock. A later
            // registration observes quiescing and cannot start execution.
            return Ok(());
        }
        tokio::time::timeout(deadline, self.completion)
            .await
            .map_err(|_| crate::errors::FollowerShutdownError::DrainDeadlineExceeded)?
            .map_err(|_| crate::errors::FollowerShutdownError::DrainEndedWithoutCompletion.into())
    }
}

impl FollowerDrain {
    pub(crate) fn install(&self, executor: Mailbox) -> Result<Option<FollowerReporter>> {
        let mut ingress = self
            .ingress
            .lock()
            .map_err(|_| crate::errors::FollowerShutdownError::IngressLockPoisoned)?;
        if ingress.installed {
            return Err(crate::errors::FollowerShutdownError::IngressAlreadyInstalled.into());
        }
        if ingress.quiescing {
            return Ok(None);
        }
        ingress.installed = true;
        ingress.executor = Some(executor);
        Ok(Some(FollowerReporter {
            ingress: Arc::clone(&self.ingress),
        }))
    }

    pub(crate) async fn finish(
        self,
        executor: impl std::future::Future<Output = Result<()>>,
        observer: impl std::future::Future<Output = Result<()>>,
    ) -> Result<FollowerQuiesceReason> {
        futures::try_join!(executor, observer)?;
        let reason = {
            let mut ingress = self
                .ingress
                .lock()
                .map_err(|_| crate::errors::FollowerShutdownError::IngressLockPoisoned)?;
            let reason = match (ingress.quiescing, ingress.reason) {
                (true, Some(reason)) => reason,
                _ => {
                    return Err(
                        crate::errors::FollowerShutdownError::DrainedWithoutShutdownRequest.into(),
                    )
                }
            };
            if matches!(reason, FollowerQuiesceReason::PromoteToValidator { .. }) {
                ingress.promotion_complete = true;
            }
            reason
        };
        match reason {
            FollowerQuiesceReason::HostShutdown => self
                .completion
                .send(())
                .map_err(|_| crate::errors::FollowerShutdownError::DrainOwnerDisappeared)?,
            // A host drain that started during promotion waits for this signal.
            // Without such a waiter, nobody needs it.
            FollowerQuiesceReason::PromoteToValidator { .. } => {
                let _ = self.completion.send(());
            }
        }
        Ok(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{channel::mpsc, FutureExt as _, StreamExt as _};

    #[tokio::test]
    async fn drain_closes_ingress_but_waits_for_both_execution_and_proofs() {
        let (control, drain) = follower_drain_pair();
        let (tx, mut rx) = mpsc::unbounded();
        let reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
        let wait = control.drain(Duration::from_secs(1));
        tokio::pin!(wait);
        assert!(wait.as_mut().now_or_never().is_none());
        assert!(reporter.is_quiescing().unwrap());
        assert!(rx.next().await.is_none());
        let (exec_tx, exec_rx) = oneshot::channel();
        let (proof_tx, proof_rx) = oneshot::channel();
        let finish = drain.finish(async { exec_rx.await.unwrap() }, async {
            proof_rx.await.unwrap()
        });
        tokio::pin!(finish);
        exec_tx.send(Ok(())).unwrap();
        assert!(finish.as_mut().now_or_never().is_none());
        assert!(wait.as_mut().now_or_never().is_none());
        proof_tx.send(Ok(())).unwrap();
        finish.await.unwrap();
        wait.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_before_registration_prevents_executor_start() {
        let (control, drain) = follower_drain_pair();
        control.drain(Duration::from_secs(1)).await.unwrap();
        let (tx, mut rx) = mpsc::unbounded();
        assert!(drain.install(Mailbox::from_sender(tx)).unwrap().is_none());
        assert!(rx.next().await.is_none());
    }

    #[tokio::test]
    async fn executor_or_proof_failure_never_acknowledges_a_clean_drain() {
        for fail_executor in [false, true] {
            let (control, drain) = follower_drain_pair();
            let (tx, _rx) = mpsc::unbounded();
            let _reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
            let wait = control.drain(Duration::from_secs(1));
            tokio::pin!(wait);
            assert!(wait.as_mut().now_or_never().is_none());
            let result = drain
                .finish(
                    async {
                        if fail_executor {
                            Err(eyre!("execution failure"))
                        } else {
                            Ok(())
                        }
                    },
                    async { Err(eyre!("certificate missing or store write failed")) },
                )
                .await;
            let message = result.unwrap_err().to_string();
            assert!(message.contains(if fail_executor {
                "execution failure"
            } else {
                "store write failed"
            }));
            assert!(wait
                .await
                .unwrap_err()
                .to_string()
                .contains("without completion"));
        }
    }

    #[tokio::test]
    async fn post_quiesce_delivery_is_neither_executed_nor_falsely_acknowledged() {
        use commonware_utils::acknowledgement::Acknowledgement as _;
        use outbe_consensus::block::ConsensusBlock;
        use reth_ethereum::{primitives::SealedBlock, Block};
        let (control, drain) = follower_drain_pair();
        let (tx, mut rx) = mpsc::unbounded();
        let mut reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
        let wait = control.drain(Duration::from_secs(1));
        tokio::pin!(wait);
        assert!(wait.as_mut().now_or_never().is_none());
        let block = Block::default().map_header(outbe_primitives::OutbeHeader::new);
        let (ack, acknowledged) = Exact::handle();
        tokio::pin!(acknowledged);
        reporter.report(Update::Block(
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(block)).into(),
            ack,
        ));
        assert!(rx.next().await.is_none());
        assert!(acknowledged.as_mut().now_or_never().is_none());
        assert_eq!(
            drain
                .finish(async { Ok(()) }, async { Ok(()) })
                .await
                .unwrap(),
            FollowerQuiesceReason::HostShutdown
        );
        wait.await.unwrap();
        assert!(acknowledged.as_mut().now_or_never().is_none());
        // Marshal owns this reporter until its actor exits. Only then may the
        // unexecuted delivery's acknowledgement be dropped, never acknowledged.
        drop(reporter);
        assert!(acknowledged.await.is_err());
    }

    #[tokio::test]
    async fn self_initiated_promotion_drains_and_makes_host_drain_a_no_op() {
        let (control, drain) = follower_drain_pair();
        let (tx, mut rx) = mpsc::unbounded();
        let reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
        reporter.initiate_promotion(42).unwrap();
        assert!(reporter.is_quiescing().unwrap());
        // The executor ingress closes exactly as in a host drain.
        assert!(rx.next().await.is_none());
        let reason = drain
            .finish(async { Ok(()) }, async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(
            reason,
            FollowerQuiesceReason::PromoteToValidator {
                finalized_height: 42
            }
        );
        // The later host drain neither waits nor fails.
        control.drain(Duration::ZERO).await.unwrap();
    }

    #[tokio::test]
    async fn host_drain_during_promotion_waits_for_the_promotion_drain() {
        let (control, drain) = follower_drain_pair();
        let (tx, _rx) = mpsc::unbounded();
        let reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
        reporter.initiate_promotion(7).unwrap();
        let wait = control.drain(Duration::from_secs(1));
        tokio::pin!(wait);
        assert!(wait.as_mut().now_or_never().is_none());
        drain
            .finish(async { Ok(()) }, async { Ok(()) })
            .await
            .unwrap();
        wait.await.unwrap();
    }

    #[tokio::test]
    async fn promotion_after_host_drain_keeps_the_shutdown_reason() {
        let (control, drain) = follower_drain_pair();
        let (tx, _rx) = mpsc::unbounded();
        let reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
        let wait = control.drain(Duration::from_secs(1));
        tokio::pin!(wait);
        assert!(wait.as_mut().now_or_never().is_none());
        reporter.initiate_promotion(9).unwrap();
        assert_eq!(
            drain
                .finish(async { Ok(()) }, async { Ok(()) })
                .await
                .unwrap(),
            FollowerQuiesceReason::HostShutdown
        );
        wait.await.unwrap();
    }

    #[tokio::test]
    async fn lost_completion_or_deadline_is_an_error() {
        let (control, drain) = follower_drain_pair();
        let (tx, _rx) = mpsc::unbounded();
        let _reporter = drain.install(Mailbox::from_sender(tx)).unwrap().unwrap();
        let error = control.drain(Duration::ZERO).await.unwrap_err();
        assert!(error.to_string().contains("deadline exceeded"));
        drop(drain);
    }
}

#[derive(Clone)]
pub(crate) struct FollowerReporter {
    ingress: Arc<Mutex<Ingress>>,
}

impl FollowerReporter {
    pub(crate) fn is_quiescing(&self) -> Result<bool> {
        Ok(self
            .ingress
            .lock()
            .map_err(|_| crate::errors::FollowerShutdownError::IngressLockPoisoned)?
            .quiescing)
    }

    /// Why ingress closed, or `None` while the follower still executes.
    pub(crate) fn quiesce_reason(&self) -> Result<Option<FollowerQuiesceReason>> {
        Ok(self
            .ingress
            .lock()
            .map_err(|_| crate::errors::FollowerShutdownError::IngressLockPoisoned)?
            .reason)
    }

    /// Close execution ingress because the validator is bootstrap-ready.
    /// The executor drains its accepted queue and closes its height stream,
    /// exactly as for a host drain. A host drain that already started wins:
    /// then this call changes nothing.
    pub(crate) fn initiate_promotion(&self, finalized_height: u64) -> Result<()> {
        let mut ingress = self
            .ingress
            .lock()
            .map_err(|_| crate::errors::FollowerShutdownError::IngressLockPoisoned)?;
        if ingress.quiescing {
            return Ok(());
        }
        ingress.quiescing = true;
        ingress.reason = Some(FollowerQuiesceReason::PromoteToValidator { finalized_height });
        ingress.executor.take();
        Ok(())
    }
}

impl Reporter for FollowerReporter {
    type Activity = MarshalUpdate;

    fn report(&mut self, activity: MarshalUpdate) -> Feedback {
        let mut ingress = self.ingress.lock().expect("follower ingress lock poisoned");
        if let Some(executor) = ingress.executor.as_mut() {
            return executor.report(activity);
        }
        assert!(
            ingress.quiescing,
            "follower reporter has no executor before shutdown"
        );
        if let Update::Block(_, ack) = activity {
            ingress.withheld.push(ack);
        }
        Feedback::Ok
    }
}
