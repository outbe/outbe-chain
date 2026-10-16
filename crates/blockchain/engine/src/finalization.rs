//! Native durable execution barrier before Marshal acknowledges a finalized block.
//! Notifications wake the waiter; fresh DB-only canonical and state progress
//! checks authorize completion. There is no auxiliary tree or persistence ledger.

use alloy_consensus::BlockHeader as _;
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use futures::{future::BoxFuture, stream::BoxStream, FutureExt, StreamExt};
use outbe_consensus::executor::actor::{FinalizedExecutionBarrier, FinalizedExecutionBlock};
use outbe_primitives::{projection::ProjectionCheckpoint, OutbeHeader};
use reth_chain_state::PersistedBlockSubscriptions;
use reth_provider::{BlockHashReader, DatabaseProviderFactory, HeaderProvider};
use reth_stages_types::StageId;
use reth_storage_api::StageCheckpointReader;
use std::{sync::Arc, time::Duration};

mod persistence;
use persistence::DurablePersistence;

/// Failure to prove native durable execution.
#[derive(Debug, thiserror::Error)]
pub enum FinalizationBarrierError {
    #[error("failed to open Reth DB-only provider: {0}")]
    OpenDbProvider(String),
    #[error("failed to read durable block {height}: {error}")]
    ReadDurableBlock { height: u64, error: String },
    #[error("failed to read durable header {height}: {error}")]
    ReadDurableHeader { height: u64, error: String },
    #[error("durable header {height}/{block_hash} is missing")]
    DurableHeaderMissing { height: u64, block_hash: B256 },
    #[error("durable header/hash index conflict at {height}: {index}/{header}")]
    DurableHeaderHashConflict {
        height: u64,
        index: B256,
        header: B256,
    },
    #[error("durable parent mismatch at {height}: expected {expected}, actual {actual}")]
    ParentMismatch {
        height: u64,
        expected: B256,
        actual: B256,
    },
    #[error("durable canonical conflict at {height}: expected {finalized}, actual {actual}")]
    DurableCanonicalConflict {
        height: u64,
        finalized: B256,
        actual: B256,
    },
    #[error("durable state absent at {height}/{block_hash}")]
    DbStateAbsent { height: u64, block_hash: B256 },
    #[error("persistence deadline {deadline:?} exceeded at {height}/{block_hash}")]
    PersistenceDeadlineExceeded {
        height: u64,
        block_hash: B256,
        deadline: Duration,
    },
    #[error("persistence stream ended before {height}/{block_hash}")]
    PersistenceStreamEnded { height: u64, block_hash: B256 },
    #[error("persistence passed {height}/{block_hash} at {notification_number}/{notification_hash} without exact durable state")]
    PersistencePassedBlock {
        height: u64,
        block_hash: B256,
        notification_number: u64,
        notification_hash: B256,
    },
}

#[derive(Clone, Copy, Debug)]
struct PersistenceWaitPolicy {
    recheck_interval: Duration,
    deadline: Duration,
}
impl Default for PersistenceWaitPolicy {
    fn default() -> Self {
        Self {
            recheck_interval: Duration::from_secs(1),
            deadline: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DurableProbe {
    Absent,
    DifferentHash(B256),
    Exact(B256),
}

/// Native header evidence read only after durable executed state reaches its height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableExecutionEvidence {
    pub block_hash: B256,
    pub parent_block_hash: B256,
    pub state_root: B256,
}

/// DB-only authority, independent of execution caches and persistence notifications.
pub trait DurableExecutionState: Send + Sync {
    fn persisted_blocks(&self) -> BoxStream<'static, BlockNumHash>;
    fn block_and_root(&self, height: u64) -> eyre::Result<Option<DurableExecutionEvidence>>;
}

/// Reads block identity and executed-state progress in the same fresh DB snapshot.
#[derive(Clone, Debug)]
pub struct RethDurableExecutionState<P> {
    provider: P,
}
impl<P> RethDurableExecutionState<P> {
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}
impl<P> DurableExecutionState for RethDurableExecutionState<P>
where
    P: PersistedBlockSubscriptions + DatabaseProviderFactory + Clone + Send + Sync + 'static,
    P::Provider: BlockHashReader + HeaderProvider<Header = OutbeHeader> + StageCheckpointReader,
{
    fn persisted_blocks(&self) -> BoxStream<'static, BlockNumHash> {
        self.provider.persisted_block_stream().boxed()
    }
    fn block_and_root(&self, height: u64) -> eyre::Result<Option<DurableExecutionEvidence>> {
        let durable = self
            .provider
            .database_provider_ro()
            .map_err(|e| FinalizationBarrierError::OpenDbProvider(e.to_string()))?;
        durable_execution_evidence(&durable, height)
    }
}

fn durable_execution_evidence<DB>(
    durable: &DB,
    height: u64,
) -> eyre::Result<Option<DurableExecutionEvidence>>
where
    DB: BlockHashReader + HeaderProvider<Header = OutbeHeader> + StageCheckpointReader,
{
    let Some(block_hash) =
        durable
            .block_hash(height)
            .map_err(|e| FinalizationBarrierError::ReadDurableBlock {
                height,
                error: e.to_string(),
            })?
    else {
        return Ok(None);
    };
    let header = durable
        .sealed_header(height)
        .map_err(|e| FinalizationBarrierError::ReadDurableHeader {
            height,
            error: e.to_string(),
        })?
        .ok_or(FinalizationBarrierError::DurableHeaderMissing { height, block_hash })?;
    if header.hash() != block_hash {
        return Err(FinalizationBarrierError::DurableHeaderHashConflict {
            height,
            index: block_hash,
            header: header.hash(),
        }
        .into());
    }
    let checkpoint = durable.get_stage_checkpoint(StageId::Finish)?;
    let finish = checkpoint.as_ref().map_or(0, |c| c.block_number);
    let state_tip = checkpoint
        .as_ref()
        .and_then(|c| c.finish_stage_checkpoint())
        .and_then(|c| c.partial_state_trie())
        .unwrap_or(finish);
    if height > state_tip {
        return Ok(None);
    }
    if height > 0 {
        let Some(parent) = durable.block_hash(height - 1)? else {
            return Ok(None);
        };
        if parent != header.parent_hash() {
            return Err(FinalizationBarrierError::ParentMismatch {
                height,
                expected: header.parent_hash(),
                actual: parent,
            }
            .into());
        }
    }
    Ok(Some(DurableExecutionEvidence {
        block_hash,
        parent_block_hash: header.parent_hash(),
        state_root: header.state_root(),
    }))
}

/// Serialized persistence gate shared by validators and followers.
pub struct RethFinalizationBarrier {
    persistence: DurablePersistence,
    finalization: Arc<futures::lock::Mutex<()>>,
}
impl std::fmt::Debug for RethFinalizationBarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RethFinalizationBarrier")
            .finish_non_exhaustive()
    }
}
impl RethFinalizationBarrier {
    /// Subscribe before finalized processing starts.
    pub fn new(state: Arc<dyn DurableExecutionState>) -> Self {
        Self::with_wait_policy(state, PersistenceWaitPolicy::default())
    }
    fn with_wait_policy(
        state: Arc<dyn DurableExecutionState>,
        wait_policy: PersistenceWaitPolicy,
    ) -> Self {
        let persisted = state.persisted_blocks();
        Self {
            persistence: DurablePersistence {
                state,
                persisted: Arc::new(futures::lock::Mutex::new(persisted)),
                wait_policy,
            },
            finalization: Arc::new(futures::lock::Mutex::new(())),
        }
    }
    async fn commit(&self, block: FinalizedExecutionBlock) -> eyre::Result<()> {
        let _serial = self.finalization.lock().await;
        match self.persistence.probe_durable(block)? {
            DurableProbe::Exact(_) => return Ok(()),
            DurableProbe::Absent | DurableProbe::DifferentHash(_) => {
                self.persistence.wait_for_exact_persistence(block).await?
            }
        }
        self.persistence
            .verify_durable(block)?
            .ok_or(FinalizationBarrierError::DbStateAbsent {
                height: block.height,
                block_hash: block.block_hash,
            })?;
        Ok(())
    }
}
impl FinalizedExecutionBarrier for RethFinalizationBarrier {
    fn commit_finalized(
        &self,
        block: FinalizedExecutionBlock,
    ) -> BoxFuture<'static, eyre::Result<()>> {
        let this = Self {
            persistence: self.persistence.clone(),
            finalization: Arc::clone(&self.finalization),
        };
        async move { this.commit(block).await }.boxed()
    }
    fn verify_checkpoint(&self, checkpoint: ProjectionCheckpoint) -> eyre::Result<()> {
        let evidence = self
            .persistence
            .state
            .block_and_root(checkpoint.block_number)?
            .ok_or(FinalizationBarrierError::DbStateAbsent {
                height: checkpoint.block_number,
                block_hash: checkpoint.block_hash,
            })?;
        if evidence.block_hash != checkpoint.block_hash {
            return Err(FinalizationBarrierError::DurableCanonicalConflict {
                height: checkpoint.block_number,
                finalized: checkpoint.block_hash,
                actual: evidence.block_hash,
            }
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
