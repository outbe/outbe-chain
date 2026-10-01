//! Backend-neutral finalized-receipt projection for Outbe off-chain data.
//!
//! This crate deliberately knows nothing about Reth or MongoDB. A node adapter
//! normalizes finalized blocks into [`FinalizedBlock`], while the projector
//! consumes only the shared off-chain storage capabilities.

mod day_apply;
mod day_lifecycle;
mod decode;
mod prepare;
mod retirement;
mod runtime_readers;
mod state;

pub use outbe_primitives::projection::{
    projection_readiness, ProjectionCheckpoint, ProjectionFailure, ProjectionFailureClass,
    ProjectionReadinessHandle, ProjectionReadinessPublisher, ProjectionStatus, WaitOutcome,
};
pub use runtime_readers::{ExecutionReadBudgetGuard, RuntimeBodyFailure, RuntimeBodyReaders};

pub use state::{
    read_projection_state, ProjectionConfig, ProjectionSource, ProjectionState,
    TributeRetentionSelector, PROJECTION_STATE_KEY, PROJECTION_STATE_NAMESPACE,
    STORAGE_SCHEMA_VERSION,
};

use std::sync::Arc;

use alloy_primitives::{Address, LogData, B256};
use outbe_compressed_entities::WwdEntityId;
use outbe_nod::NodRepositoryError;
use outbe_offchain_storage::{
    AtomicWriteBatch, DayDatabases, StorageError, StorageReaderHandle, StorageWriterHandle,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::TributeRepositoryError;
use thiserror::Error;

use state::{contains_unmanaged_data, state_batch};
pub(crate) use state::{decode_state, state_key, state_namespace};

/// Backend-neutral finalized log, including its canonical block-global index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedLog {
    pub log_index: u64,
    pub emitter: Address,
    pub data: LogData,
}

/// Backend-neutral successful or reverted receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedReceipt {
    pub tx_hash: B256,
    pub transaction_index: u64,
    pub success: bool,
    pub logs: Vec<FinalizedLog>,
}

/// Complete normalized receipt input for one exact finalized block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedBlock {
    pub number: u64,
    pub hash: B256,
    pub receipts: Vec<FinalizedReceipt>,
}

/// Prepared mutations for one successful receipt containing projection events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedReceipt {
    tx_hash: B256,
    transaction_index: u64,
    batch: AtomicWriteBatch,
}

impl PreparedReceipt {
    #[must_use]
    pub const fn tx_hash(&self) -> B256 {
        self.tx_hash
    }

    #[must_use]
    pub const fn transaction_index(&self) -> u64 {
        self.transaction_index
    }

    #[must_use]
    pub const fn batch(&self) -> &AtomicWriteBatch {
        &self.batch
    }
}

/// A fully decoded and simulated block. Constructed only after prepare succeeds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBlock {
    checkpoint: ProjectionCheckpoint,
    receipts: Vec<PreparedReceipt>,
    day_retirements: Vec<DayRetirement>,
}

/// One Tribute worldwide day leaving the live projection in this block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DayRetirement {
    Drop(u32),
    Retain { day: u32, lease: B256 },
}

impl PreparedBlock {
    #[must_use]
    pub const fn checkpoint(&self) -> ProjectionCheckpoint {
        self.checkpoint
    }

    #[must_use]
    pub fn receipts(&self) -> &[PreparedReceipt] {
        &self.receipts
    }
}

/// Result of applying one finalized block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionOutcome {
    Applied {
        checkpoint: ProjectionCheckpoint,
        receipt_batches: usize,
    },
    AlreadyApplied(ProjectionCheckpoint),
}

/// Durable shared database plus the per-day Tribute and Nod databases.
#[derive(Clone)]
pub struct DayDatabaseRoute {
    /// Open day databases for this off-chain root.
    pub databases: Arc<DayDatabases>,
    /// Reader for the shared database. Legacy keys and the migration cursor live here.
    pub durable_reader: StorageReaderHandle,
    /// Writer for the shared database.
    pub durable_writer: StorageWriterHandle,
}

/// Deterministic projector over shared backend-neutral storage capabilities.
pub struct OffchainDataProjection {
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
    state: ProjectionState,
    tribute_retention_selector: Option<Arc<dyn TributeRetentionSelector>>,
    day_route: Option<DayDatabaseRoute>,
    partition_retirement: bool,
}

impl OffchainDataProjection {
    /// Opens a managed database or initializes an empty one.
    pub fn open(
        config: ProjectionConfig,
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
    ) -> Result<Self, ProjectionError> {
        let state = match read_projection_state(config, reader.clone())? {
            Some(state) => state,
            None => {
                if contains_unmanaged_data(&reader)? {
                    return Err(ProjectionError::UnmanagedProjectionData);
                }
                let state = ProjectionState {
                    chain_id: config.chain_id,
                    genesis_hash: config.genesis_hash,
                    storage_schema_version: STORAGE_SCHEMA_VERSION,
                    start_block: config.start_block,
                    checkpoint: None,
                };
                writer.apply_atomic(&state_batch(&state)?)?;
                state
            }
        };
        Ok(Self {
            reader,
            writer,
            state,
            tribute_retention_selector: None,
            day_route: None,
            partition_retirement: false,
        })
    }

    /// Uses datasource-neutral physical partition retirement in finalized batches.
    pub fn enable_partition_retirement(&mut self) {
        self.partition_retirement = true;
    }

    /// Routes Tribute and Nod bodies into one database per worldwide day.
    ///
    /// A directory still present for a dropped or retired day is removed here.
    pub fn set_day_route(&mut self, route: DayDatabaseRoute) -> Result<(), ProjectionError> {
        day_lifecycle::sweep(&route)?;
        self.day_route = Some(route);
        Ok(())
    }

    /// Day routing configured for this projector, when the node opened RocksDB.
    #[must_use]
    pub fn day_route(&self) -> Option<&DayDatabaseRoute> {
        self.day_route.as_ref()
    }

    /// Opens the projector with the node-owned active-pin selector.
    pub fn open_with_retention_selector(
        config: ProjectionConfig,
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
        selector: Arc<dyn TributeRetentionSelector>,
    ) -> Result<Self, ProjectionError> {
        let mut projection = Self::open(config, reader, writer)?;
        projection.tribute_retention_selector = Some(selector);
        Ok(projection)
    }

    #[must_use]
    pub const fn state(&self) -> &ProjectionState {
        &self.state
    }

    /// Applies every receipt mutation and the checkpoint in one backend transaction.
    pub fn apply_prepared(
        &mut self,
        prepared: PreparedBlock,
    ) -> Result<ProjectionOutcome, ProjectionError> {
        self.apply_prepared_with_batch(prepared)
            .map(|(outcome, _batch)| outcome)
    }

    /// Applies one prepared block to this projector and returns the exact same
    /// atomic batch for ordered delivery to a separate durable adapter.
    pub fn apply_prepared_with_batch(
        &mut self,
        prepared: PreparedBlock,
    ) -> Result<(ProjectionOutcome, AtomicWriteBatch), ProjectionError> {
        match self.validate_next_block(
            prepared.checkpoint.block_number,
            prepared.checkpoint.block_hash,
        )? {
            NextBlock::AlreadyApplied(checkpoint) => {
                return Ok((
                    ProjectionOutcome::AlreadyApplied(checkpoint),
                    AtomicWriteBatch::new(),
                ));
            }
            NextBlock::Apply => {}
        }
        let next_state = ProjectionState {
            checkpoint: Some(prepared.checkpoint),
            ..self.state.clone()
        };
        let mut block_batch = AtomicWriteBatch::new();
        for receipt in &prepared.receipts {
            block_batch.extend(receipt.batch.operations().iter().cloned());
        }
        let state = state_batch(&next_state)?;
        let block_batch = match &self.day_route {
            Some(route) => {
                let shared = day_apply::write_day_operations(&route.databases, block_batch)?;
                day_lifecycle::finish_retirements(route, &prepared.day_retirements, shared, state)?
            }
            None => {
                if self.partition_retirement {
                    for retirement in &prepared.day_retirements {
                        let (day, mark) = match retirement {
                            DayRetirement::Drop(day) => {
                                (*day, outbe_tribute::TributeDayMark::Retired)
                            }
                            DayRetirement::Retain { day, lease } => {
                                (*day, outbe_tribute::TributeDayMark::Retained(*lease))
                            }
                        };
                        block_batch.push(outbe_tribute::tribute_day_mark_operation(day, mark)?);
                        block_batch.retire_scope(outbe_tribute::partitioning::day_scope(day)?);
                    }
                }
                block_batch.extend(state.operations().iter().cloned());
                block_batch.validate()?;
                block_batch
            }
        };
        self.writer.apply_atomic(&block_batch)?;
        self.state = next_state;
        Ok((
            ProjectionOutcome::Applied {
                checkpoint: prepared.checkpoint,
                receipt_batches: prepared.receipts.len(),
            },
            block_batch,
        ))
    }

    /// Prepares and applies one exact finalized block.
    pub fn project_block(
        &mut self,
        block: &FinalizedBlock,
    ) -> Result<ProjectionOutcome, ProjectionError> {
        if let Some(route) = &self.day_route {
            day_lifecycle::sweep(route)?;
        }
        if let NextBlock::AlreadyApplied(checkpoint) =
            self.validate_next_block(block.number, block.hash)?
        {
            return Ok(ProjectionOutcome::AlreadyApplied(checkpoint));
        }
        let prepared = self.prepare_block(block)?;
        self.apply_prepared(prepared)
    }

    fn validate_next_block(&self, number: u64, hash: B256) -> Result<NextBlock, ProjectionError> {
        match self.state.checkpoint {
            Some(checkpoint) if checkpoint.block_number == number => {
                if checkpoint.block_hash == hash {
                    Ok(NextBlock::AlreadyApplied(checkpoint))
                } else {
                    Err(ProjectionError::CheckpointMismatch {
                        block_number: number,
                        expected: checkpoint.block_hash,
                        actual: hash,
                    })
                }
            }
            Some(checkpoint) => {
                let expected = checkpoint.block_number.checked_add(1).ok_or(
                    ProjectionError::NonSequentialBlock {
                        expected: checkpoint.block_number,
                        actual: number,
                    },
                )?;
                if number != expected {
                    return Err(ProjectionError::NonSequentialBlock {
                        expected,
                        actual: number,
                    });
                }
                Ok(NextBlock::Apply)
            }
            None if number == self.state.start_block => Ok(NextBlock::Apply),
            None => Err(ProjectionError::NonSequentialBlock {
                expected: self.state.start_block,
                actual: number,
            }),
        }
    }
}

#[derive(Clone, Copy)]
enum NextBlock {
    Apply,
    AlreadyApplied(ProjectionCheckpoint),
}

/// Stable projector failures; no backend-specific type crosses this boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProjectionError {
    #[error("off-chain storage failure: {0}")]
    Storage(#[from] StorageError),
    #[error("Tribute repository failure: {0}")]
    Tribute(#[from] TributeRepositoryError),
    #[error("Nod repository failure: {0}")]
    Nod(#[from] NodRepositoryError),
    #[error("failed to encode projection state")]
    StateEncode(#[source] postcard::Error),
    #[error("failed to decode projection state")]
    StateDecode(#[source] postcard::Error),
    #[error("corrupt projection state: {0}")]
    CorruptProjectionState(String),
    #[error("projection schema mismatch: expected {expected}, found {actual}")]
    ProjectionSchemaMismatch { expected: u32, actual: u32 },
    #[error("projection identity does not match configured chain")]
    ProjectionIdentityMismatch {
        expected: ProjectionConfig,
        actual_chain_id: u64,
        actual_genesis_hash: B256,
        actual_start_block: u64,
    },
    #[error("body/index records exist without projection state")]
    UnmanagedProjectionData,
    #[error(
        "checkpoint hash mismatch at block {block_number}: expected {expected}, found {actual}"
    )]
    CheckpointMismatch {
        block_number: u64,
        expected: B256,
        actual: B256,
    },
    #[error("non-sequential block: expected {expected}, found {actual}")]
    NonSequentialBlock { expected: u64, actual: u64 },
    #[error("invalid transaction order: expected {expected}, found {actual}")]
    InvalidTransactionOrder { expected: u64, actual: u64 },
    #[error("transaction index does not fit u64")]
    TransactionIndexOverflow,
    #[error("invalid block-global log order: expected {expected}, found {actual}")]
    InvalidLogOrder { expected: u64, actual: u64 },
    #[error("block-global log index overflow")]
    LogIndexOverflow,
    #[error("recognized projection log appears in a failed receipt at {0:?}")]
    ProjectionLogInFailedReceipt(Box<ProjectionSource>),
    #[error("malformed recognized projection event at {event_source:?}: {reason}")]
    MalformedProjectionEvent {
        event_source: Box<ProjectionSource>,
        reason: String,
    },
    #[error("malformed projection metadata: {0}")]
    MalformedProjectionMetadata(String),
    #[error("managed {entity} primary record has no projection metadata")]
    MissingProjectionMetadata { entity: &'static str },
    #[error("OCOMP retention selector failed for day {worldwide_day}: {reason}")]
    RetentionSelector {
        worldwide_day: WorldwideDay,
        reason: String,
    },
    #[error(
        "OCOMP retention selector returned day {selected} for requested partition {requested}"
    )]
    RetentionPinDayMismatch {
        requested: WorldwideDay,
        selected: WorldwideDay,
    },
    #[error("corrupt projected body: {0}")]
    CorruptProjectedBody(String),
    #[error(
        "{entity} {identity} commitment transition expected previous {expected_previous}, found {actual}"
    )]
    CommitmentTransitionMismatch {
        entity: &'static str,
        identity: WwdEntityId,
        expected_previous: B256,
        actual: B256,
    },
    #[error("tribute {tribute_id} changes after its worldwide day was retired in the same block")]
    TributeStoredAfterDayRetirement { tribute_id: WwdEntityId },
}

/// Composition of domain-owned entity routing, shared by node and exporters.
pub fn entity_partition_routing() -> Result<
    std::sync::Arc<dyn outbe_offchain_storage::PartitionRouting>,
    outbe_offchain_storage::StorageError,
> {
    use outbe_offchain_storage::partitioned::routing::{RoutingRegistry, SharedRouting};
    let mut registry = RoutingRegistry::new(std::sync::Arc::new(SharedRouting(
        outbe_offchain_storage::StorageScope::shared("system")?,
    )));
    outbe_nod::partitioning::register(&mut registry)?;
    outbe_tribute::partitioning::register(&mut registry)?;
    Ok(std::sync::Arc::new(registry))
}
