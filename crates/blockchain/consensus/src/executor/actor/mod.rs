//! Executor actor - sends forkchoice updates and handles finalization.
//!
//! Tracks the canonical chain head and finalized block, sending FCU updates
//! to Reth's beacon engine. Receives finalized blocks from marshal via the
//! Reporter trait and acknowledges after successful EL processing.
//!
//! Internal forkchoice state is updated only after a successful FCU response
//! from the engine.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime},
};

use super::ingress::{Mailbox, Message};
use crate::ancestry_readiness::AncestryReadiness;
use alloy_primitives::B256;
use commonware_consensus::types::Height;
use commonware_runtime::{Clock, Handle, Metrics, Spawner};
use commonware_utils::channel::oneshot;
use futures::StreamExt;
use outbe_primitives::projection::ProjectionReadinessHandle;
use outbe_primitives::OutbePayloadTypes;
use reth_node_builder::ConsensusEngineHandle;
use tracing::error;
use tracing::info;
use tracing::warn;

/// Type alias for the engine handle (standard Ethereum engine types).
type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;

/// The executor actor.
pub struct ExecutorActor<E> {
    context: E,
    engine: EngineHandle,
    state: LastCanonicalized,
    mailbox_rx: futures::channel::mpsc::UnboundedReceiver<Message>,
    // Intentionally `tokio::sync::mpsc`: this height-signal channel is created and
    // consumed cross-crate by `outbe-engine` (`stack.rs`). It is a plain channel
    // with no timer/spawn dependency - runtime-agnostic, so it does not pull the
    // tokio reactor onto the executor's deterministic-capable path.
    execution_finalized_height_tx: Option<tokio::sync::mpsc::UnboundedSender<u64>>,
    projection_readiness: ProjectionReadinessHandle,
    ocomp_readiness: Option<ProjectionReadinessHandle>,
    finalized_ce_committer: Option<Arc<dyn FinalizedCeCommitter>>,
    ancestry_readiness: Option<AncestryReadiness>,
    fcu_heartbeat_interval: Duration,
    next_fcu_heartbeat_deadline: SystemTime,
    pending_finalized_subscriptions: BTreeMap<Height, Vec<oneshot::Sender<()>>>,
}

impl<E> ExecutorActor<E>
where
    E: Clock + Metrics + Spawner + Send + Sync + 'static,
{
    /// Create a new executor actor with recovered finalized state.
    pub fn new(
        context: E,
        engine: EngineHandle,
        genesis_hash: B256,
        last_finalized_height: u64,
        last_finalized_hash: B256,
        projection_readiness: ProjectionReadinessHandle,
        execution_finalized_height_tx: Option<tokio::sync::mpsc::UnboundedSender<u64>>,
    ) -> (Self, Mailbox) {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let mailbox = Mailbox::from_sender(tx);
        let state = LastCanonicalized::from_recovered(
            genesis_hash,
            last_finalized_height,
            last_finalized_hash,
        );
        let fcu_heartbeat_interval = crate::config::DEFAULT_FCU_HEARTBEAT_INTERVAL;
        let next_fcu_heartbeat_deadline = next_deadline(context.current(), fcu_heartbeat_interval);
        let actor = Self {
            context,
            engine,
            state,
            mailbox_rx: rx,
            execution_finalized_height_tx,
            projection_readiness,
            ocomp_readiness: None,
            finalized_ce_committer: None,
            ancestry_readiness: None,
            fcu_heartbeat_interval,
            next_fcu_heartbeat_deadline,
            pending_finalized_subscriptions: BTreeMap::new(),
        };
        (actor, mailbox)
    }

    pub fn with_ancestry_readiness(mut self, readiness: AncestryReadiness) -> Self {
        self.ancestry_readiness = Some(readiness);
        self
    }

    /// Installs the FullNode-only OCOMP replay barrier. Validators deliberately
    /// leave this unset, preserving their existing execution path.
    #[must_use]
    pub fn with_ocomp_readiness(mut self, readiness: ProjectionReadinessHandle) -> Self {
        self.ocomp_readiness = Some(readiness);
        self
    }

    /// Reconcile the startup state after marshal has exposed the exact
    /// application finalization record for the canonical execution head.
    ///
    /// Marshal must be started before that record can be queried, while its
    /// reporter needs this actor's mailbox. This startup-only builder closes
    /// that ordering loop without allowing a speculative execution head to be
    /// treated as finalized: the caller is responsible for validating the
    /// recovered finalization digest before invoking it.
    #[must_use]
    pub fn with_recovered_finalized_state(
        mut self,
        genesis_hash: B256,
        finalized_height: u64,
        finalized_hash: B256,
    ) -> Self {
        self.state =
            LastCanonicalized::from_recovered(genesis_hash, finalized_height, finalized_hash);
        self
    }

    /// Installs the mandatory compressed-storage barrier for live node wiring.
    #[must_use]
    pub fn with_finalized_ce_committer(mut self, committer: Arc<dyn FinalizedCeCommitter>) -> Self {
        self.finalized_ce_committer = Some(committer);
        self
    }

    /// Start the executor under the Commonware runtime supervision tree.
    pub fn start(
        self,
        marshal: crate::marshal_types::MarshalMailbox,
        last_consensus_finalized: Height,
    ) -> Handle<eyre::Result<()>> {
        let context = self.context.child("executor");
        context.spawn(move |_| self.run(marshal, last_consensus_finalized))
    }

    /// Run the executor event loop with startup backfill.
    ///
    /// Returns `Err` only on an unrecoverable fault: a *finalized* block (already
    /// agreed by consensus) that this node cannot apply locally. That means our
    /// state has diverged from the finalized chain, so the node must fail fast -
    /// the supervisor treats this `Err` as fatal and shuts the node down with the
    /// structured cause, rather than the silent fall-through that previously
    /// surfaced only as an opaque marshal "did not acknowledge" panic.
    async fn run(
        mut self,
        marshal: crate::marshal_types::MarshalMailbox,
        last_consensus_finalized: Height,
    ) -> eyre::Result<()> {
        // Startup backfill: execution behind consensus.
        let execution_height = self.state.finalized_height;
        if let Some(readiness) = &self.ancestry_readiness {
            readiness.set_target_height(last_consensus_finalized.get());
            readiness.note_ready_height(execution_height.get());
        }
        if last_consensus_finalized > execution_height {
            info!(
                execution_height = execution_height.get(),
                consensus_height = last_consensus_finalized.get(),
                "backfilling execution from marshal"
            );
            for h in (execution_height.get() + 1)..=last_consensus_finalized.get() {
                let height = Height::new(h);
                match marshal.get_block(height).await {
                    Some(block) => {
                        let digest = crate::digest::Digest(block.block_hash());
                        match self.handle_finalize_inner(height, digest, block).await {
                            Ok(()) => {
                                self.notify_finalized_subscribers(height);
                                self.notify_execution_finalized(height);
                            }
                            Err(error) => {
                                error!(
                                    %height, %digest, %error,
                                    "backfill: finalized block failed local execution; \
                                     finalized state diverged - failing fast"
                                );
                                return Err(eyre::eyre!(
                                    "executor backfill cannot apply finalized block at \
                                     height {height} digest {digest}: {error}"
                                ));
                            }
                        }
                    }
                    None => {
                        // The backfill range is `(execution_height, last_consensus_finalized]`
                        // - every height here is <= the finalized height marshal itself
                        // reported, so marshal must be able to produce it. A `None` means
                        // marshal's archive is inconsistent (claims finalized to N but cannot
                        // serve M <= N). Skipping would leave a non-contiguous execution gap
                        // (the next block's new_payload fails on the missing parent, or the
                        // node silently stalls below consensus height), so this is an
                        // unrecoverable fault - fail fast like the execution-failure branch.
                        error!(
                            height = h,
                            consensus_height = last_consensus_finalized.get(),
                            "backfill: marshal is missing a finalized block at or below its \
                             reported finalized height; archive is inconsistent - failing fast"
                        );
                        return Err(eyre::eyre!(
                            "executor backfill: marshal missing finalized block at height {h} \
                             (<= reported finalized height {}); cannot reconstruct contiguous \
                             execution state",
                            last_consensus_finalized.get()
                        ));
                    }
                }
            }
            info!("backfill complete");
        } else if execution_height > last_consensus_finalized {
            warn!(
                execution_height = execution_height.get(),
                consensus_height = last_consensus_finalized.get(),
                "execution ahead of consensus - skipping backfill"
            );
        } else {
            info!("execution and consensus at same height - no backfill needed");
        }

        self.run_live_loop().await
    }

    async fn run_live_loop(&mut self) -> eyre::Result<()> {
        // Live event loop. Mailbox messages stay biased ahead of heartbeat so
        // queued marshal updates are not overtaken by timer work.
        loop {
            let heartbeat = self.context.sleep_until(self.next_fcu_heartbeat_deadline);
            let mut heartbeat = std::pin::pin!(heartbeat);

            // `commonware_macros::select!` is biased (top-to-bottom): mailbox
            // messages stay ahead of the heartbeat timer, matching the prior
            // `tokio::select! { biased; .. }`. Runs on both the tokio and the
            // deterministic runtimes (no tokio reactor dependency).
            commonware_macros::select! {
                msg = self.mailbox_rx.next() => {
                    let Some(msg) = msg else {
                        info!("executor actor mailbox closed, exiting");
                        return Ok(());
                    };
                    // A fatal (diverged-finalized-state) message propagates up and
                    // ends the loop, signalling the supervisor to shut the node down.
                    self.handle_message(msg).await?;
                },

                _ = &mut heartbeat => {
                    self.send_fcu_heartbeat().await;
                },
            }
        }
    }

    async fn handle_message(&mut self, msg: Message) -> eyre::Result<()> {
        match msg {
            Message::CanonicalizeHead(req) => {
                self.canonicalize(
                    HeadOrFinalized::Head,
                    req.height,
                    req.digest,
                    MaybeBuild::JustCanonicalize {
                        response: req.response,
                    },
                )
                .await;
                Ok(())
            }
            Message::CanonicalizeAndBuild(req) => {
                self.canonicalize(
                    HeadOrFinalized::Head,
                    req.height,
                    req.digest,
                    MaybeBuild::AlsoBuild {
                        attributes: req.attributes,
                        response: req.response,
                    },
                )
                .await;
                Ok(())
            }
            Message::MarshalUpdate(update) => self.handle_marshal_update(*update).await,
            Message::SubscribeFinalized(req) => {
                self.handle_subscribe_finalized(req.height, req.response);
                Ok(())
            }
        }
    }

    fn handle_subscribe_finalized(&mut self, height: Height, response: oneshot::Sender<()>) {
        if self.state.finalized_height >= height {
            let _ = response.send(());
            return;
        }
        self.pending_finalized_subscriptions
            .entry(height)
            .or_default()
            .push(response);
    }

    fn reset_fcu_heartbeat_deadline(&mut self) {
        self.next_fcu_heartbeat_deadline =
            next_deadline(self.context.current(), self.fcu_heartbeat_interval);
    }
}

#[cfg(test)]
mod tests;

mod forkchoice;
use forkchoice::{next_deadline, LastCanonicalized, MaybeBuild};

mod readiness;
use readiness::{wait_for_finalized_parent, wait_for_optional_ocomp_parent, HeadOrFinalized};

mod recovery;
pub use recovery::RecoveredForkchoiceAttempt;

mod finalization;
pub use finalization::{FinalizedCeBlock, FinalizedCeCommitter};
