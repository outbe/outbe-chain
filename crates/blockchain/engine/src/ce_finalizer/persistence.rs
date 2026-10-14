//! DB-only persistence proof and advisory notification waiting.
use super::{
    validate_durable_header_evidence, CePersistenceWaitPolicy, DurableCeProbe, DurableCeState,
};
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use futures::{stream::BoxStream, StreamExt};
use outbe_consensus::executor::actor::FinalizedCeBlock;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct DurableCePersistence {
    pub(super) state: Arc<dyn DurableCeState>,
    pub(super) persisted: Arc<futures::lock::Mutex<BoxStream<'static, BlockNumHash>>>,
    pub(super) wait_policy: CePersistenceWaitPolicy,
}

impl DurableCePersistence {
    pub(super) fn verify_durable(&self, block: FinalizedCeBlock) -> eyre::Result<Option<B256>> {
        match self.probe_durable(block)? {
            DurableCeProbe::Absent => Ok(None),
            DurableCeProbe::Exact(root) => Ok(Some(root)),
            DurableCeProbe::DifferentHash(actual) => {
                Err(crate::errors::CeFinalizerError::DurableCanonicalConflict {
                    height: block.height,
                    finalized: block.block_hash,
                    actual,
                }
                .into())
            }
        }
    }

    pub(super) fn probe_durable(&self, block: FinalizedCeBlock) -> eyre::Result<DurableCeProbe> {
        let Some(evidence) = self.state.block_and_root(block.height)? else {
            return Ok(DurableCeProbe::Absent);
        };
        if evidence.block_hash != block.block_hash {
            return Ok(DurableCeProbe::DifferentHash(evidence.block_hash));
        }
        Ok(DurableCeProbe::Exact(validate_durable_header_evidence(
            block.height,
            evidence,
        )?))
    }

    pub(super) async fn wait_for_exact_persistence(
        &self,
        block: FinalizedCeBlock,
    ) -> eyre::Result<()> {
        let mut persisted = self.persisted.lock().await;
        let deadline = tokio::time::sleep(self.wait_policy.deadline);
        tokio::pin!(deadline);
        let first_recheck = tokio::time::Instant::now() + self.wait_policy.recheck_interval;
        let mut rechecks =
            tokio::time::interval_at(first_recheck, self.wait_policy.recheck_interval);
        rechecks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = &mut deadline => {
                    // Resolve the boundary race in favor of durable proof: a block
                    // becoming visible at the deadline still completes online.
                    if matches!(self.probe_durable(block)?, DurableCeProbe::Exact(_)) {
                        return Ok(());
                    }
                    return Err(crate::errors::CeFinalizerError::PersistenceDeadlineExceeded {
                        height: block.height,
                        block_hash: block.block_hash,
                        deadline: self.wait_policy.deadline,
                    }.into());
                }
                _ = rechecks.tick() => {
                    if matches!(self.probe_durable(block)?, DurableCeProbe::Exact(_)) {
                        return Ok(());
                    }
                }
                notification = persisted.next() => {
                    let Some(notification) = notification else {
                        return Err(crate::errors::CeFinalizerError::PersistenceStreamEnded {
                            height: block.height,
                            block_hash: block.block_hash,
                        }.into());
                    };
                    if notification.number < block.height {
                        continue;
                    }
                    if notification.number == block.height
                        && notification.hash == block.block_hash
                    {
                        // Notifications are advisory. Accept the wake only after
                        // the exact target is visible through a fresh DB-only read.
                        if matches!(self.probe_durable(block)?, DurableCeProbe::Exact(_)) {
                            return Ok(());
                        }
                        continue;
                    }
                    if notification.number == block.height {
                        // Reth can persist a speculative canonical head at H before
                        // consensus finalizes a different block at the same H. The
                        // finalized block was already submitted through newPayload and
                        // FCU, so wait for its replacement persistence notification.
                        // Treating the old same-height hash as "passed" kills an honest
                        // validator during an ordinary pre-finalization reorg.
                        continue;
                    }
                    if matches!(self.probe_durable(block)?, DurableCeProbe::Exact(_)) {
                        // This is a watch stream, so a slow receiver may observe a
                        // later durable tip. This loop accepts the target only after a
                        // fresh DB-only transaction proves its exact canonical identity.
                        return Ok(());
                    }
                    return Err(crate::errors::CeFinalizerError::PersistencePassedBlock {
                        height: block.height,
                        block_hash: block.block_hash,
                        notification_number: notification.number,
                        notification_hash: notification.hash,
                    }.into());
                }
            }
        }
    }
}
