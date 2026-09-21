use super::apply_durable_projection_write_before;
use super::publish_status;
use super::require_finalized_checkpoint;
use super::DurableProjectionWrite;
use super::ProjectionRuntime;
use super::ProjectionRuntimeRecoveryHandle;
use super::ReadyOffchainDataProjection;
use super::PROJECTION_RECOVERY_DEADLINE;
use crate::finalized_frame::FinalizedFrame;
use alloy_consensus::transaction::TxHashRef;
use alloy_primitives::B256;
use eyre::bail;
use eyre::Context;
use outbe_offchain_data::FinalizedBlock;
use outbe_offchain_data::FinalizedLog;
use outbe_offchain_data::FinalizedReceipt;
use outbe_offchain_data::ProjectionFailure;
use outbe_offchain_data::ProjectionOutcome;
use outbe_offchain_data::ProjectionStatus;
use outbe_offchain_data::RuntimeBodyFailure;
use outbe_primitives::projection::ProjectionCheckpoint;
use reth_primitives_traits::Block;
use reth_primitives_traits::BlockBody;
use reth_primitives_traits::Receipt as RethReceipt;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::RwLockReadGuard;
use std::sync::RwLockWriteGuard;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FinalizedTarget {
    pub(super) number: u64,
    pub(super) hash: B256,
}

impl FinalizedTarget {
    pub(super) const fn new(number: u64, hash: B256) -> Self {
        Self { number, hash }
    }
}

/// Orders retained-input projection commits against OCOMP garbage collection.
///
/// Projection holds the shared side from pin selection through the durable
/// offchain storage commit. The retention worker briefly takes the exclusive side only
/// while it durably claims a lease for collection; physical deletion happens
/// after the exclusive guard is released.
#[derive(Default)]
pub struct ProjectionRetentionFence {
    gate: RwLock<()>,
}

impl ProjectionRetentionFence {
    pub(super) fn projection_guard(&self) -> eyre::Result<RwLockReadGuard<'_, ()>> {
        self.gate
            .read()
            .map_err(|_| eyre::eyre!("OCOMP projection/GC fence is poisoned"))
    }

    pub(crate) fn gc_claim_guard(&self) -> Result<RwLockWriteGuard<'_, ()>, &'static str> {
        self.gate
            .write()
            .map_err(|_| "OCOMP projection/GC fence is poisoned")
    }
}

/// Deep sink for projecting an already-read finalized frame into durable Mongo state.
///
/// The sink owns the logical overlay and the single-writer lease inherited from
/// [`ReadyOffchainDataProjection`]. [`Self::project_frame`] does not return a new checkpoint until
/// the exact atomic batch has committed through the durable writer. Frames below the durable
/// checkpoint are accepted as restart replay; a replay at the checkpoint height must have the
/// exact durable hash.
pub struct FinalizedProjectionSink {
    runtime: ProjectionRuntime,
    durable_checkpoint: Option<ProjectionCheckpoint>,
    provider_recovery_floor: Option<ProjectionCheckpoint>,
    retention_fence: Option<Arc<ProjectionRetentionFence>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizedTargetReconciliationV1 {
    AwaitingProviderRecovery,
    Process {
        target: ProjectionCheckpoint,
        recovered_floor: Option<ProjectionCheckpoint>,
    },
}

pub(super) fn normalize_finalized_block<B, R>(
    block_number: u64,
    block_hash: B256,
    block: &B,
    receipts: &[R],
) -> eyre::Result<FinalizedBlock>
where
    B: Block,
    R: RethReceipt,
{
    let transactions = block.body().transactions();
    if transactions.len() != receipts.len() {
        bail!(
            "canonical block {} has {} transactions but {} receipts",
            block_number,
            transactions.len(),
            receipts.len()
        );
    }

    let mut normalized_receipts = Vec::with_capacity(receipts.len());
    let mut next_log_index = 0_u64;
    for (transaction_index, (transaction, receipt)) in transactions.iter().zip(receipts).enumerate()
    {
        let transaction_index = u64::try_from(transaction_index)
            .map_err(|_| eyre::eyre!("transaction index does not fit u64"))?;
        let mut logs = Vec::with_capacity(receipt.logs().len());
        for log in receipt.logs() {
            logs.push(FinalizedLog {
                log_index: next_log_index,
                emitter: log.address,
                data: log.data.clone(),
            });
            next_log_index = next_log_index
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("block-global log index overflow"))?;
        }
        normalized_receipts.push(FinalizedReceipt {
            tx_hash: *transaction.tx_hash(),
            transaction_index,
            success: receipt.status(),
            // Every log is retained in receipt order, including unrelated logs, so these indices
            // remain the canonical block-global indices.
            logs,
        });
    }

    Ok(FinalizedBlock {
        number: block_number,
        hash: block_hash,
        receipts: normalized_receipts,
    })
}

impl FinalizedProjectionSink {
    #[must_use]
    pub fn new(ready: ReadyOffchainDataProjection) -> Self {
        let retention_fence = Arc::clone(&ready.retention_fence);
        Self::from_runtime_with_retention_fence(ProjectionRuntime::new(ready), retention_fence)
    }

    #[must_use]
    pub const fn durable_checkpoint(&self) -> Option<ProjectionCheckpoint> {
        self.durable_checkpoint
    }

    pub fn runtime_failure_receiver(
        &self,
    ) -> eyre::Result<tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>> {
        self.runtime
            .runtime_failure_receiver
            .as_ref()
            .cloned()
            .ok_or_else(|| eyre::eyre!("projection body-read failure receiver is unavailable"))
    }

    pub fn runtime_recovery_handle(&self) -> eyre::Result<ProjectionRuntimeRecoveryHandle> {
        Ok(ProjectionRuntimeRecoveryHandle {
            writer: self.runtime.writer.clone(),
            failure_sender: self
                .runtime
                .runtime_failure_sender
                .as_ref()
                .cloned()
                .ok_or_else(|| {
                    eyre::eyre!("projection body-read recovery sender is unavailable")
                })?,
        })
    }

    pub fn publish_failure(&self, failure: ProjectionFailure) {
        self.runtime
            .readiness_publisher
            .publish(ProjectionStatus::Fatal {
                checkpoint: self.durable_checkpoint,
                error: failure,
            });
    }

    /// Reconciles a sampled provider finalized target with the durable projection floor.
    ///
    /// Reth can temporarily expose no finalized marker, or an older marker, while restoring its
    /// forkchoice state after restart. That state is recoverable only when the durable checkpoint
    /// still has the exact canonical hash validated at startup. Readiness remains closed until the
    /// provider reaches the floor again; a same-height identity conflict remains fatal.
    pub fn reconcile_finalized_target(
        &mut self,
        target: Option<ProjectionCheckpoint>,
    ) -> eyre::Result<FinalizedTargetReconciliationV1> {
        let Some(durable) = self.durable_checkpoint else {
            return Ok(match target {
                Some(target) => FinalizedTargetReconciliationV1::Process {
                    target,
                    recovered_floor: None,
                },
                None => {
                    publish_status(
                        &self.runtime.readiness_publisher,
                        ProjectionStatus::CatchingUp { checkpoint: None },
                        None,
                    );
                    FinalizedTargetReconciliationV1::AwaitingProviderRecovery
                }
            });
        };
        let provider_target =
            target.map(|target| FinalizedTarget::new(target.block_number, target.block_hash));
        let reconciled = require_finalized_checkpoint(durable, provider_target)?;
        let Some(reconciled) = reconciled else {
            self.provider_recovery_floor = Some(durable);
            publish_status(
                &self.runtime.readiness_publisher,
                ProjectionStatus::CatchingUp {
                    checkpoint: Some(durable),
                },
                provider_target,
            );
            return Ok(FinalizedTargetReconciliationV1::AwaitingProviderRecovery);
        };
        Ok(FinalizedTargetReconciliationV1::Process {
            target: ProjectionCheckpoint {
                block_number: reconciled.number,
                block_hash: reconciled.hash,
            },
            recovered_floor: self.provider_recovery_floor.take(),
        })
    }

    /// Publishes projection readiness against the coordinator's sampled finalized target.
    pub fn publish_progress(&self, target: ProjectionCheckpoint) -> eyre::Result<()> {
        let durable = self.durable_checkpoint.unwrap_or(ProjectionCheckpoint {
            block_number: 0,
            block_hash: self.runtime.projection_config.genesis_hash,
        });
        if durable.block_number > target.block_number
            || (durable.block_number == target.block_number
                && durable.block_hash != target.block_hash)
        {
            bail!("durable projection checkpoint is ahead of or conflicts with finalized target");
        }
        self.runtime
            .readiness_publisher
            .publish(if durable == target {
                ProjectionStatus::Ready {
                    checkpoint: durable,
                }
            } else {
                ProjectionStatus::CatchingUp {
                    checkpoint: Some(durable),
                }
            });
        Ok(())
    }

    /// Projects one shared finalized frame and returns the exact durable projection checkpoint.
    ///
    /// This method is synchronous because the configured storage interface is synchronous. An
    /// async coordinator should call it on its blocking worker. Durable write failures retry the
    /// same atomic batch and never expose logical progress as durable P.
    pub fn project_frame(&mut self, frame: &FinalizedFrame) -> eyre::Result<ProjectionCheckpoint> {
        self.project_frame_until(
            frame,
            std::time::Instant::now() + PROJECTION_RECOVERY_DEADLINE,
        )
    }

    pub fn project_frame_until(
        &mut self,
        frame: &FinalizedFrame,
        deadline: std::time::Instant,
    ) -> eyre::Result<ProjectionCheckpoint> {
        let identity = frame.identity();
        if let Some(checkpoint) = self.durable_checkpoint {
            if identity.number < checkpoint.block_number {
                return Ok(checkpoint);
            }
            if identity.number == checkpoint.block_number {
                if identity.hash != checkpoint.block_hash {
                    bail!(
                        "finalized frame hash {} conflicts with durable projection hash {} at height {}",
                        identity.hash,
                        checkpoint.block_hash,
                        identity.number
                    );
                }
                return Ok(checkpoint);
            }
        }

        let retention_fence = self.retention_fence.clone();
        let _retention_guard = retention_fence
            .as_ref()
            .map(|fence| fence.projection_guard())
            .transpose()?;
        let normalized = normalize_finalized_block(
            identity.number,
            identity.hash,
            frame.block(),
            frame.receipts(),
        )?;
        let overlay = self.runtime.overlay.clone();
        let prepared = self
            .runtime
            .projector
            .prepare_block(&normalized)
            .wrap_err_with(|| format!("project finalized frame {}", identity.number))?;
        let (projected, durable_batch) = self
            .runtime
            .projector
            .apply_prepared_with_batch(prepared)
            .wrap_err_with(|| format!("apply logical finalized frame {}", identity.number))?;
        let projected = match projected {
            ProjectionOutcome::Applied { checkpoint, .. } => checkpoint,
            ProjectionOutcome::AlreadyApplied(checkpoint) => {
                bail!(
                    "logical projection checkpoint {} ({}) is ahead of durable frame-sink authority",
                    checkpoint.block_number,
                    checkpoint.block_hash
                );
            }
        };
        if projected.block_number != identity.number || projected.block_hash != identity.hash {
            bail!(
                "projector returned checkpoint {} ({}) after projecting shared frame {} ({})",
                projected.block_number,
                projected.block_hash,
                identity.number,
                identity.hash
            );
        }
        let overlay_ack = overlay
            .as_ref()
            .map(|overlay| (Arc::clone(overlay), overlay.current_generation()));
        apply_durable_projection_write_before(
            &self.runtime.writer,
            &DurableProjectionWrite {
                checkpoint: FinalizedTarget::new(projected.block_number, projected.block_hash),
                batch: durable_batch,
                overlay_ack,
            },
            deadline,
        )?;
        self.durable_checkpoint = Some(projected);
        Ok(projected)
    }

    fn from_runtime_with_retention_fence(
        runtime: ProjectionRuntime,
        retention_fence: Arc<ProjectionRetentionFence>,
    ) -> Self {
        Self::from_runtime_inner(runtime, Some(retention_fence))
    }

    pub(super) fn from_runtime_inner(
        runtime: ProjectionRuntime,
        retention_fence: Option<Arc<ProjectionRetentionFence>>,
    ) -> Self {
        let durable_checkpoint = runtime.projector.state().checkpoint;
        Self {
            runtime,
            durable_checkpoint,
            // A reopened durable projection must prove this exact canonical floor once more at
            // the live provider boundary before processing resumes. This also covers a provider
            // marker that jumps from behind the floor to above it before the first poll.
            provider_recovery_floor: durable_checkpoint,
            retention_fence,
        }
    }
}
