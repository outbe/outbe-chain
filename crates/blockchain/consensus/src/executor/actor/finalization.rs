use super::{wait_for_optional_ocomp_parent, ExecutorActor, HeadOrFinalized, MaybeBuild};

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

/// Exact finalized block identity handed to native execution persistence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedExecutionBlock {
    pub height: u64,
    pub block_hash: B256,
    pub parent_block_hash: B256,
}

/// Finalization barrier installed by the node integration.
///
/// The returned future completes only after DB-only canonical and executed-state
/// verification. Persistence notifications are advisory. The executor awaits it
/// before acknowledging Marshal.
pub trait FinalizedExecutionBarrier: Send + Sync {
    fn commit_finalized(
        &self,
        block: FinalizedExecutionBlock,
    ) -> BoxFuture<'static, eyre::Result<()>>;

    /// Verify the certified native recovery anchor before participation.
    fn verify_checkpoint(&self, checkpoint: ProjectionCheckpoint) -> eyre::Result<()>;
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
            Ok(response) if response.is_invalid() || response.is_syncing() => {
                warn!(
                    ?response,
                    "forkchoice-update heartbeat returned non-valid status"
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

        let fcu_result = self
            .engine
            .fork_choice_updated(new_state.forkchoice, maybe_build.attributes().cloned())
            .await;
        self.reset_fcu_heartbeat_deadline();
        let Ok(fcu_response) = fcu_result else {
            let e = fcu_result.expect_err("checked Err");
            warn!(%e, "failed to send forkchoice update");
            maybe_build.send_error(crate::errors::ExecutorError::FcuFailed(format!("{e}")).into());
            return;
        };

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
        } else if fcu_response.is_invalid() {
            error!(
                ?head_or_finalized,
                ?fcu_response,
                finalized_block_hash = %new_state.forkchoice.finalized_block_hash,
                finalized_height = %new_state.finalized_height,
                "forkchoice update returned invalid status"
            );
            maybe_build.send_error(
                crate::errors::ExecutorError::FcuInvalid(format!(
                    "{:?}",
                    fcu_response.payload_status
                ))
                .into(),
            );
            return;
        }

        // Success - respond and commit.
        match maybe_build {
            MaybeBuild::JustCanonicalize { response } => {
                let _ = response.send(Ok(()));
            }
            MaybeBuild::AlsoBuild { response, .. } => {
                let Some(id) = fcu_response.payload_id else {
                    let _ = response.send(Err(crate::errors::ExecutorError::FcuMissingPayloadId {
                        payload_status: format!("{:?}", fcu_response.payload_status),
                        latest_valid_hash: format!(
                            "{:?}",
                            fcu_response.payload_status.latest_valid_hash
                        ),
                        head: new_state.forkchoice.head_block_hash,
                        safe: new_state.forkchoice.safe_block_hash,
                        finalized: new_state.forkchoice.finalized_block_hash,
                        head_height: new_state.head_height,
                        finalized_height: new_state.finalized_height,
                    }
                    .into()));
                    return;
                };
                let _ = response.send(Ok(id));
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
                if (height == self.state.finalized_height
                    && digest.0 == self.state.forkchoice.finalized_block_hash)
                    || (height == Height::zero() && digest.0 == self.genesis_hash)
                {
                    info!(
                        %height,
                        %digest,
                        "marshal-delivered block already canonical; acknowledging without reexecution"
                    );
                    ack.acknowledge();
                    self.notify_finalized_subscribers(height);
                    self.notify_execution_finalized(height);
                    return Ok(());
                }
                if height == Height::zero() {
                    return Err(crate::errors::ExecutorError::UnexpectedGenesisAnchor {
                        digest,
                        finalized_height: self.state.finalized_height,
                        expected: self.genesis_hash,
                    }
                    .into());
                }
                info!(
                    %height,
                    %digest,
                    "marshal-delivered block: handle_finalize_inner start"
                );
                if let Err(error) = self
                    .handle_finalize_inner(height, digest, (*block).clone())
                    .await
                {
                    // A finalized block (already agreed by consensus) that we
                    // cannot apply locally means our state diverged from the
                    // finalized chain. This is unrecoverable. Fail fast
                    // deterministically with the structured cause. We deliberately
                    // do NOT acknowledge: we did not process the block.
                    error!(
                        %height, %digest, %error,
                        "finalized block failed local execution; \
                         finalized state diverged - failing fast"
                    );
                    return Err(crate::errors::ExecutorError::ApplyFinalizedBlockFailed {
                        height,
                        digest,
                        reason: error,
                    }
                    .into());
                }
                info!(%height, %digest, "marshal-delivered block finalized and acked");
                ack.acknowledge();
                self.notify_finalized_subscribers(height);
                self.notify_execution_finalized(height);
                Ok(())
            }
            commonware_consensus::marshal::Update::Tip(round, height, digest) => {
                self.verification.get_mut().finalized(round, height, digest);
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
    /// dropped). A `Syncing` payload status is not a failure. It proceeds to
    /// canonicalization like the prior behavior.
    pub(super) async fn handle_finalize_inner(
        &mut self,
        height: Height,
        digest: crate::digest::Digest,
        block: crate::block::ConsensusBlock,
    ) -> eyre::Result<()> {
        let parent_height = height
            .get()
            .checked_sub(1)
            .ok_or(crate::errors::ExecutorError::ExecuteGenesisThroughSuccessor(digest))?;
        let parent_checkpoint = ProjectionCheckpoint {
            block_number: parent_height,
            block_hash: block.parent_hash(),
        };
        wait_for_optional_ocomp_parent(self.ocomp_readiness.clone(), parent_checkpoint).await?;

        let execution_data =
            OutbeExecutionData::new(std::sync::Arc::new(block.clone().into_inner()));

        if crate::test_faults::should_drop_new_payload_for_test(height) {
            warn!(
                height = %height,
                digest = %digest,
                "test-marshal-drop: skipping finalized new_payload before FCU"
            );
        } else {
            let status = self.engine.new_payload(execution_data).await.map_err(|e| {
                crate::errors::ExecutorError::SendFinalizedBlockFailed {
                    height,
                    digest,
                    reason: format!("{e}"),
                }
            })?;
            if status.is_valid() {
                info!(
                    height = %height,
                    digest = %digest,
                    ?status,
                    "finalized block accepted by execution layer"
                );
            } else if status.is_syncing()
                || matches!(
                    status.status,
                    alloy_rpc_types_engine::PayloadStatusEnum::Accepted
                )
            {
                warn!(
                    height = %height,
                    digest = %digest,
                    ?status,
                    "execution layer syncing/accepted on finalized block"
                );
            } else {
                return Err(crate::errors::ExecutorError::FinalizedBlockRejected {
                    height,
                    digest,
                    status: format!("{status:?}"),
                }
                .into());
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
        let canonicalize_result = response_rx.await.map_err(|_| {
            crate::errors::ExecutorError::CanonicalizeResponseDropped { height, digest }
        })?;
        canonicalize_result.map_err(|error| {
            crate::errors::ExecutorError::CanonicalizeFinalizedFailed {
                height,
                digest,
                reason: error,
            }
        })?;
        if let Some(committer) = &self.finalized_execution_barrier {
            committer
                .commit_finalized(FinalizedExecutionBlock {
                    height: height.get(),
                    block_hash: block.block_hash(),
                    parent_block_hash: block.parent_hash(),
                })
                .await?;
        }
        Ok(())
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
