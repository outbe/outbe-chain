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
                eyre::bail!(
                    "durable canonical conflict at height {}: finalized={}, Reth={}",
                    block.height,
                    block.block_hash,
                    actual
                );
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
                    eyre::bail!(
                        "finalized CE persistence deadline exceeded for {}/{} after {:?}",
                        block.height,
                        block.block_hash,
                        self.wait_policy.deadline
                    );
                }
                _ = rechecks.tick() => {
                    if matches!(self.probe_durable(block)?, DurableCeProbe::Exact(_)) {
                        return Ok(());
                    }
                }
                notification = persisted.next() => {
                    let Some(notification) = notification else {
                        return Err(eyre::eyre!(
                            "Reth persistence stream ended before finalized CE block {}/{}",
                            block.height,
                            block.block_hash
                        ));
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
                        // later durable tip. The target is accepted only after a fresh
                        // DB-only transaction proves its exact canonical identity.
                        return Ok(());
                    }
                    eyre::bail!(
                        "Reth persistence passed finalized CE block {}/{} with {}/{}",
                        block.height,
                        block.block_hash,
                        notification.number,
                        notification.hash
                    );
                }
            }
        }
    }
}
