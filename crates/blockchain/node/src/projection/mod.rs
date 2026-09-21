//! Reth ExEx adapter for finalized offchain-data projection.
//!
//! Canonical-chain notifications are deliberately only drained here. The provider's finalized
//! block signal is the sole authority that permits projection writes.

use alloy_primitives::B256;
use eyre::{bail, Context};
use outbe_offchain_data::OffchainDataProjection;
use outbe_offchain_data::ProjectionConfig;
use outbe_offchain_data::ProjectionFailure;
use outbe_offchain_data::ProjectionReadinessHandle;
use outbe_offchain_data::ProjectionReadinessPublisher;
use outbe_offchain_data::ProjectionStatus;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_data::TributeRetentionSelector;
use outbe_offchain_storage::OpenedStorage;
use outbe_offchain_storage::PendingOverlayStorage;
use outbe_offchain_storage::StorageConfig;
use outbe_offchain_storage::StorageError;
use outbe_offchain_storage::StorageErrorKind;
use outbe_offchain_storage::StorageOwnershipGuard;
use outbe_offchain_storage::StorageReaderHandle;
use outbe_offchain_storage::StorageWriterHandle;
use outbe_primitives::{
    chain::network_for_chain_id,
    projection::{projection_readiness, ProjectionCheckpoint},
};
use outbe_tribute::RetainedTributeWriter;
use std::sync::Arc;
use std::time::Duration;

pub use outbe_offchain_data::RuntimeBodyFailure;

const PROJECTION_RETRY_INTERVAL: Duration = Duration::from_secs(1);
pub const PROJECTION_RECOVERY_DEADLINE: Duration = Duration::from_secs(8);

/// Structured terminal condition reported to the top-level node lifecycle owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionExit {
    pub failure: ProjectionFailure,
}

/// Complete startup configuration for the required finalized offchain-data projection.
#[derive(Clone)]
pub struct OffchainDataProjectionConfig {
    /// EVM chain identity recorded in the managed projection state.
    pub chain_id: u64,
    /// Canonical genesis hash recorded in the managed projection state.
    pub genesis_hash: B256,
    /// Shared backend configuration, including the first projected block.
    pub storage: StorageConfig,
}

/// Projection instance whose offchain storage connection, topology, and managed state passed preflight.
pub struct PreparedOffchainDataProjection {
    projector: OffchainDataProjection,
    storage: OpenedStorage,
    overlay: Arc<PendingOverlayStorage>,
    readiness_publisher: ProjectionReadinessPublisher,
    readiness: ProjectionReadinessHandle,
    runtime_failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
    runtime_failure_receiver: tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>,
    retention_fence: Arc<ProjectionRetentionFence>,
}

impl PreparedOffchainDataProjection {
    /// Typed read-only capabilities injected into EVM execution.
    #[must_use]
    pub fn runtime_body_readers(&self) -> RuntimeBodyReaders {
        let reader: StorageReaderHandle = self.overlay.clone();
        RuntimeBodyReaders::new_supervised(reader, self.runtime_failure_sender.clone())
    }

    /// Backend-neutral exact-checkpoint readiness used by local execution gates.
    #[must_use]
    pub fn readiness(&self) -> ProjectionReadinessHandle {
        self.readiness.clone()
    }

    /// Durable release capability backed by the exact storage owned by this projection.
    #[must_use]
    pub fn retained_tribute_writer(&self) -> Arc<RetainedTributeWriter> {
        let reader = self.storage.reader.clone();
        let writer = self.storage.writer.clone();
        Arc::new(RetainedTributeWriter::new(reader, writer))
    }

    /// Exact process-local fence shared by projection and retained-input GC.
    #[must_use]
    pub fn retention_fence(&self) -> Arc<ProjectionRetentionFence> {
        Arc::clone(&self.retention_fence)
    }
}

/// Projection instance whose available canonical checkpoint identity passed startup checks.
pub struct ReadyOffchainDataProjection {
    projector: OffchainDataProjection,
    readiness_publisher: ProjectionReadinessPublisher,
    projection_config: ProjectionConfig,
    _reader: StorageReaderHandle,
    overlay: Arc<PendingOverlayStorage>,
    writer: StorageWriterHandle,
    writer_lease: StorageOwnershipGuard,
    runtime_failure_sender: tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>,
    runtime_failure_receiver: tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>,
    retention_fence: Arc<ProjectionRetentionFence>,
}

/// Connects to offchain storage and validates storage prerequisites before Reth component initialization.
pub fn prepare_offchain_data_projection(
    config: OffchainDataProjectionConfig,
) -> eyre::Result<PreparedOffchainDataProjection> {
    prepare_offchain_data_projection_inner(config, None)
}

/// Prepares projection with the node-owned OCOMP retention selector installed on both the
/// durable preflight and logical frame sink.
pub fn prepare_offchain_data_projection_with_retention(
    config: OffchainDataProjectionConfig,
    selector: Arc<dyn TributeRetentionSelector>,
) -> eyre::Result<PreparedOffchainDataProjection> {
    prepare_offchain_data_projection_inner(config, Some(selector))
}

fn prepare_offchain_data_projection_inner(
    config: OffchainDataProjectionConfig,
    selector: Option<Arc<dyn TributeRetentionSelector>>,
) -> eyre::Result<PreparedOffchainDataProjection> {
    validate_projection_network(config.chain_id)?;
    if config.storage.start_block != 1 {
        bail!(
            "ADR-005 requires projection start_block 1, found {}",
            config.storage.start_block
        );
    }

    let started = std::time::Instant::now();
    loop {
        let remaining = PROJECTION_RECOVERY_DEADLINE.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            bail!("offchain storage startup recovery exceeded the eight-second total deadline");
        }
        let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(1);
        let attempt_config = config.clone();
        let attempt_selector = selector.clone();
        std::thread::Builder::new()
            .name("offchain-startup".to_owned())
            .spawn(move || {
                let _ = attempt_tx.send(prepare_projection_attempt(
                    &attempt_config,
                    attempt_selector,
                ));
            })
            .wrap_err("spawn offchain storage startup validation worker")?;
        let attempt = match attempt_rx.recv_timeout(remaining) {
            Ok(attempt) => attempt,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                bail!("offchain storage startup recovery exceeded the eight-second total deadline");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("offchain storage startup validation worker exited unexpectedly");
            }
        };
        match attempt {
            Ok((storage, overlay, projector)) => {
                let initial = match projector.state().checkpoint {
                    Some(checkpoint) => ProjectionStatus::CatchingUp {
                        checkpoint: Some(checkpoint),
                    },
                    None => ProjectionStatus::Starting,
                };
                let (readiness_publisher, readiness) = projection_readiness(
                    ProjectionCheckpoint {
                        block_number: 0,
                        block_hash: config.genesis_hash,
                    },
                    initial,
                );
                let (runtime_failure_sender, runtime_failure_receiver) =
                    tokio::sync::watch::channel(None);
                return Ok(PreparedOffchainDataProjection {
                    projector,
                    storage,
                    overlay,
                    readiness_publisher,
                    readiness,
                    runtime_failure_sender,
                    runtime_failure_receiver,
                    retention_fence: Arc::new(ProjectionRetentionFence::default()),
                });
            }
            Err(error)
                if error.is_unavailable() && started.elapsed() < PROJECTION_RECOVERY_DEADLINE =>
            {
                let remaining = PROJECTION_RECOVERY_DEADLINE.saturating_sub(started.elapsed());
                std::thread::sleep(PROJECTION_RETRY_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error.into_eyre()),
        }
    }
}

fn validate_projection_network(chain_id: u64) -> eyre::Result<()> {
    if network_for_chain_id(chain_id).is_none() {
        bail!("offchain projection rejects unknown Outbe chain ID {chain_id}");
    }
    Ok(())
}

enum PrepareProjectionError {
    Storage(StorageError),
    Projection(outbe_offchain_data::ProjectionError),
}

impl PrepareProjectionError {
    fn is_unavailable(&self) -> bool {
        match self {
            Self::Storage(error)
            | Self::Projection(outbe_offchain_data::ProjectionError::Storage(error)) => {
                error.kind() == StorageErrorKind::Unavailable
            }
            Self::Projection(_) => false,
        }
    }

    fn into_eyre(self) -> eyre::Report {
        match self {
            Self::Storage(error) => eyre::Report::new(error),
            Self::Projection(error) => eyre::Report::new(error),
        }
    }
}

struct ProjectionRuntime {
    projector: OffchainDataProjection,
    readiness_publisher: ProjectionReadinessPublisher,
    projection_config: ProjectionConfig,
    _reader: StorageReaderHandle,
    overlay: Option<Arc<PendingOverlayStorage>>,
    writer: StorageWriterHandle,
    _writer_lease: Option<StorageOwnershipGuard>,
    runtime_failure_sender: Option<tokio::sync::watch::Sender<Option<RuntimeBodyFailure>>>,
    runtime_failure_receiver: Option<tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>>,
}

impl ProjectionRuntime {
    fn new(ready: ReadyOffchainDataProjection) -> Self {
        Self {
            projector: ready.projector,
            readiness_publisher: ready.readiness_publisher,
            projection_config: ready.projection_config,
            _reader: ready._reader,
            overlay: Some(ready.overlay),
            writer: ready.writer,
            _writer_lease: Some(ready.writer_lease),
            runtime_failure_sender: Some(ready.runtime_failure_sender),
            runtime_failure_receiver: Some(ready.runtime_failure_receiver),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_support;

mod containment;
#[cfg(test)]
use containment::evaluate_ocomp_projection_containment;
pub use containment::{ocomp_projection_contains, OcompProjectionContainment};

mod startup;
#[cfg(test)]
use startup::open_logical_projection;
pub use startup::validate_offchain_data_checkpoint;
use startup::{prepare_projection_attempt, require_finalized_checkpoint};

mod durable_writer;
pub use durable_writer::ProjectionWriteDeadlineError;
use durable_writer::{apply_durable_projection_write_before, DurableProjectionWrite};

mod status;
pub use status::projection_frame_failure_class;
#[cfg(test)]
use status::readiness_checkpoint;
use status::{publish_status, storage_failure_class};

mod sink;
#[cfg(test)]
use sink::normalize_finalized_block;
use sink::FinalizedTarget;
pub use sink::{
    FinalizedProjectionSink, FinalizedTargetReconciliationV1, ProjectionRetentionFence,
};

mod recovery;
pub use recovery::{ProjectionRuntimeRecoveryHandle, ProjectionRuntimeRecoveryV1};
