use super::{
    wait_for_finalized_parent, wait_for_optional_ocomp_parent, ExecutorActor, HeadOrFinalized,
    MaybeBuild,
};

use crate::digest::Digest;
use alloy_primitives::B256;
use commonware_consensus::types::Height;
use commonware_runtime::Clock;
use commonware_runtime::Metrics;
use commonware_runtime::Spawner;
use commonware_utils::acknowledgement::Acknowledgement;
use commonware_utils::channel::oneshot;
use futures::future::BoxFuture;
use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::OutbeExecutionData;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

/// Exact finalized block identity handed to compressed-storage persistence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedCeBlock {
    pub height: u64,
    pub block_hash: B256,
    pub parent_block_hash: B256,
}

/// Finalization barrier installed by the node integration.
///
/// The returned future completes only after Reth's durable notification,
/// DB-only canonical/root verification, and the atomic CE MDBX commit. The
/// executor deliberately awaits it before acknowledging Marshal.
pub trait FinalizedCeCommitter: Send + Sync {
    fn commit_finalized(&self, block: FinalizedCeBlock) -> BoxFuture<'static, eyre::Result<()>>;
}

impl<E> ExecutorActor<E>
where
    E: Clock + Metrics + Spawner + Send + Sync + 'static,
{
    pub(super) async fn send_fcu_heartbeat(&mut self) {
        debug!(
            head_block_hash = %self.state.forkchoice.head_block_hash,
            safe_block_hash = %self.state.forkchoice.safe_block_hash,
            finalized_block_hash = %self.state.forkchoice.finalized_block_hash,
            head_height = %self.state.head_height,
            finalized_height = %self.state.finalized_height,
            "sending forkchoice-update heartbeat"
        );

        match self
            .engine
            .fork_choice_updated(self.state.forkchoice, None)
            .await
        {
            Ok(response) if response.is_invalid() => {
                warn!(
                    ?response,
                    "forkchoice-update heartbeat returned invalid status"
                );
            }
            Ok(response) if response.is_syncing() => {
                warn!(
                    ?response,
                    "forkchoice-update heartbeat returned syncing status"
                );
            }
            Ok(response) => {
                debug!(?response, "forkchoice-update heartbeat completed");
            }
            Err(error) => {
                warn!(%error, "forkchoice-update heartbeat failed");
            }
        }
        self.reset_fcu_heartbeat_deadline();
    }

    /// Unified canonicalization method.
    ///
    /// Computes new forkchoice state, sends FCU to engine, and only commits
    /// the state update after a successful response.
    pub(super) async fn canonicalize(
        &mut self,
        head_or_finalized: HeadOrFinalized,
        height: Height,
        digest: Digest,
        maybe_build: MaybeBuild,
    ) {
        let new_state = match head_or_finalized {
            HeadOrFinalized::Head => self.state.update_head(height, digest),
            HeadOrFinalized::Finalized => self.state.update_finalized(height, digest),
        };

        // Skip FCU if no state change AND we're not building a payload.
        if new_state == self.state {
            if let MaybeBuild::JustCanonicalize { response } = maybe_build {
                let _ = response.send(Ok(()));
                return;
            }
        }

        info!(
            head_block_hash = %new_state.forkchoice.head_block_hash,
            head_height = %new_state.head_height,
            finalized_block_hash = %new_state.forkchoice.finalized_block_hash,
            finalized_height = %new_state.finalized_height,
            "sending forkchoice-update",
        );

        let fcu_response = match self
            .engine
            .fork_choice_updated(new_state.forkchoice, maybe_build.attributes().cloned())
            .await
        {
            Err(e) => {
                self.reset_fcu_heartbeat_deadline();
                warn!(%e, "failed to send forkchoice update");
                maybe_build.send_error(eyre::eyre!("FCU failed: {e}"));
                return;
            }
            Ok(response) => response,
        };
        self.reset_fcu_heartbeat_deadline();

        if fcu_response.is_syncing() {
            warn!(
                ?fcu_response,
                head_block_hash = %new_state.forkchoice.head_block_hash,
                safe_block_hash = %new_state.forkchoice.safe_block_hash,
                finalized_block_hash = %new_state.forkchoice.finalized_block_hash,
                head_height = %new_state.head_height,
                finalized_height = %new_state.finalized_height,
                "forkchoice update returned syncing status"
            );
        } else if fcu_response.is_valid() {
            info!(
                ?fcu_response,
                head_block_hash = %new_state.forkchoice.head_block_hash,
                finalized_block_hash = %new_state.forkchoice.finalized_block_hash,
                head_height = %new_state.head_height,
                finalized_height = %new_state.finalized_height,
                "forkchoice update returned valid status"
            );
        }

        if fcu_response.is_invalid() {
            warn!(?fcu_response, "forkchoice update returned invalid status");
            maybe_build.send_error(eyre::eyre!(
                "FCU returned invalid: {:?}",
                fcu_response.payload_status
            ));
            return;
        }

        // Success - respond and commit.
        match maybe_build {
            MaybeBuild::JustCanonicalize { response } => {
                let _ = response.send(Ok(()));
            }
            MaybeBuild::AlsoBuild { response, .. } => {
                match fcu_response.payload_id {
                    Some(id) => {
                        let _ = response.send(Ok(id));
                    }
                    None => {
                        let _ = response.send(Err(eyre::eyre!(
                            "FCU did not return payload_id: payload_status={:?} latest_valid_hash={:?} head={} safe={} finalized={} head_height={} finalized_height={}",
                            fcu_response.payload_status,
                            fcu_response.payload_status.latest_valid_hash,
                            new_state.forkchoice.head_block_hash,
                            new_state.forkchoice.safe_block_hash,
                            new_state.forkchoice.finalized_block_hash,
                            new_state.head_height,
                            new_state.finalized_height,
                        )));
                        // Don't commit state if we didn't get a payload_id.
                        return;
                    }
                }
            }
        }
        self.state = new_state;
    }

    /// Handle a marshal update (finalized block delivery or tip notification).
    pub(super) async fn handle_marshal_update(
        &mut self,
        update: crate::marshal_types::MarshalUpdate,
    ) -> eyre::Result<()> {
        match update {
            commonware_consensus::marshal::Update::Block(block, ack) => {
                let height = Height::new(block.number());
                let digest = Digest(block.block_hash());
                if height == self.state.finalized_height
                    && digest.0 == self.state.forkchoice.finalized_block_hash
                {
                    info!(
                        %height,
                        %digest,
                        "marshal-delivered block already canonical after recovery; acknowledging without reexecution"
                    );
                    ack.acknowledge();
                    self.notify_finalized_subscribers(height);
                    self.notify_execution_finalized(height);
                    return Ok(());
                }
                if height == Height::zero() {
                    let expected = self.state.forkchoice.finalized_block_hash;
                    return Err(eyre::eyre!(
                        "marshal delivered unexpected genesis anchor: digest {digest}, \
                         canonical finalized height {}, canonical hash {expected}",
                        self.state.finalized_height
                    ));
                }
                info!(
                    %height,
                    %digest,
                    "marshal-delivered block: handle_finalize_inner start"
                );
                match self
                    .handle_finalize_inner(height, digest, (*block).clone())
                    .await
                {
                    Ok(()) => {
                        info!(%height, %digest, "marshal-delivered block finalized and acked");
                        // Acknowledge ONLY after the block is durably applied. The
                        // marshal `Exact` waiter resolves once every cloned ack is
                        // acknowledged.
                        ack.acknowledge();
                        self.notify_finalized_subscribers(height);
                        self.notify_execution_finalized(height);
                        Ok(())
                    }
                    Err(error) => {
                        // A finalized block (already agreed by consensus) that we
                        // cannot apply locally means our state has diverged from the
                        // finalized chain - unrecoverable. Fail fast deterministically
                        // with the structured cause. We deliberately do NOT
                        // acknowledge: the block was not processed, and acking would
                        // lie to the marshal's progress tracking (letting it prune a
                        // block we still need).
                        //
                        // Note: the unacknowledged `ack` still cancels on drop, and
                        // upstream marshal `handle_ack` treats a canceled ack as fatal
                        // (`panic!("application did not acknowledge...")`). With the
                        // runtime's `catch_panics`, that panic is CAUGHT (it does not
                        // abort the process) - so it is NOT the shutdown driver and does
                        // NOT pre-empt this path. The authoritative shutdown driver is
                        // the structured `Err` returned here: it propagates out of
                        // run_live_loop/run, the supervisor select treats the executor
                        // exit as fatal, and the node shuts down with the cause below.
                        // The marshal panic may still appear in logs (a caught,
                        // less-informative secondary symptom) - this `error!` precedes
                        // it with the real reason.
                        error!(
                            %height, %digest, %error,
                            "finalized block failed local execution; \
                             finalized state diverged - failing fast"
                        );
                        Err(eyre::eyre!(
                            "executor cannot apply finalized block at \
                             height {height} digest {digest}: {error}"
                        ))
                    }
                }
            }
            commonware_consensus::marshal::Update::Tip(round, height, digest) => {
                debug!(
                    %round, %height, %digest,
                    "marshal tip update"
                );
                Ok(())
            }
        }
    }

    /// Process a finalized block through the execution layer.
    ///
    /// Returns `Err` when the finalized block cannot be applied (execution layer
    /// rejected it, the engine call failed, or canonicalization failed/was
    /// dropped). A `Syncing` payload status is not a failure - it proceeds to
    /// canonicalization like the prior behavior.
    pub(super) async fn handle_finalize_inner(
        &mut self,
        height: Height,
        digest: crate::digest::Digest,
        block: crate::block::ConsensusBlock,
    ) -> eyre::Result<()> {
        let parent_height = height.get().checked_sub(1).ok_or_else(|| {
            eyre::eyre!(
                "cannot execute finalized genesis block through successor path: digest {digest}"
            )
        })?;
        wait_for_finalized_parent(
            self.projection_readiness.clone(),
            ProjectionCheckpoint {
                block_number: parent_height,
                block_hash: block.parent_hash(),
            },
        )
        .await?;
        wait_for_optional_ocomp_parent(
            self.ocomp_readiness.clone(),
            ProjectionCheckpoint {
                block_number: parent_height,
                block_hash: block.parent_hash(),
            },
        )
        .await?;

        let execution_data =
            OutbeExecutionData::new(std::sync::Arc::new(block.clone().into_inner()));

        if crate::test_faults::should_drop_new_payload_for_test(height) {
            warn!(
                height = %height,
                digest = %digest,
                "test-marshal-drop: skipping finalized new_payload before FCU"
            );
        } else {
            match self.engine.new_payload(execution_data).await {
                Ok(status) => {
                    if status.is_valid() {
                        info!(
                            height = %height,
                            digest = %digest,
                            ?status,
                            "finalized block accepted by execution layer"
                        );
                    } else if status.is_syncing() {
                        warn!(
                            height = %height,
                            digest = %digest,
                            ?status,
                            "execution layer syncing on finalized block"
                        );
                    }
                    if !status.is_valid() && !status.is_syncing() {
                        return Err(eyre::eyre!(
                            "finalized block rejected by execution layer at \
                             height {height} digest {digest}: status={status:?}"
                        ));
                    }
                }
                Err(e) => {
                    return Err(eyre::eyre!(
                        "failed to send finalized block to execution layer at \
                         height {height} digest {digest}: {e}"
                    ));
                }
            }
        }

        // Finalize via the unified canonicalize path (commit-after-success).
        let (response_tx, response_rx) = oneshot::channel();
        self.canonicalize(
            HeadOrFinalized::Finalized,
            height,
            digest,
            MaybeBuild::JustCanonicalize {
                response: response_tx,
            },
        )
        .await;
        match response_rx.await {
            Ok(Ok(())) => {
                if let Some(committer) = &self.finalized_ce_committer {
                    committer
                        .commit_finalized(FinalizedCeBlock {
                            height: height.get(),
                            block_hash: block.block_hash(),
                            parent_block_hash: block.parent_hash(),
                        })
                        .await?;
                }
                Ok(())
            }
            Ok(Err(error)) => Err(eyre::eyre!(
                "failed to canonicalize finalized block at \
                 height {height} digest {digest}: {error}"
            )),
            Err(_) => Err(eyre::eyre!(
                "executor canonicalize response dropped for finalized block at \
                 height {height} digest {digest}"
            )),
        }
    }

    pub(super) fn notify_execution_finalized(&self, height: Height) {
        if let Some(readiness) = &self.ancestry_readiness {
            let was_ready = readiness.is_ready();
            readiness.note_ready_height(height.get());
            if !was_ready && readiness.is_ready() {
                info!(
                    current_height = height.get(),
                    target_height = readiness.target_height(),
                    "marshal ancestry gate opened after executor finalized required height"
                );
            }
        }
        if let Some(tx) = &self.execution_finalized_height_tx {
            let _ = tx.send(height.get());
        }
    }

    pub(super) fn notify_finalized_subscribers(&mut self, height: Height) {
        let pending = std::mem::take(&mut self.pending_finalized_subscriptions);
        for (target_height, mut subscribers) in pending {
            if target_height <= height {
                for response in subscribers.drain(..) {
                    let _ = response.send(());
                }
            } else {
                self.pending_finalized_subscriptions
                    .insert(target_height, subscribers);
            }
        }
    }
}
