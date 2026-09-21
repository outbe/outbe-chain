use super::publish_status;
use super::FinalizedTarget;
use super::OffchainDataProjectionConfig;
use super::PrepareProjectionError;
use super::PreparedOffchainDataProjection;
use super::ReadyOffchainDataProjection;
use eyre::bail;
use eyre::Context;
use metrics::gauge;
use outbe_offchain_data::OffchainDataProjection;
use outbe_offchain_data::ProjectionConfig;
use outbe_offchain_data::ProjectionStatus;
use outbe_offchain_data::TributeRetentionSelector;
use outbe_offchain_storage::OpenedStorage;
use outbe_offchain_storage::PendingOverlayStorage;
use outbe_offchain_storage::StorageProvider;
use outbe_offchain_storage::StorageReaderHandle;
use outbe_primitives::projection::ProjectionCheckpoint;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;
use std::sync::Arc;
use tracing::info;

pub(super) fn prepare_projection_attempt(
    config: &OffchainDataProjectionConfig,
    selector: Option<Arc<dyn TributeRetentionSelector>>,
) -> Result<
    (
        OpenedStorage,
        Arc<PendingOverlayStorage>,
        OffchainDataProjection,
    ),
    PrepareProjectionError,
> {
    let projection_config = ProjectionConfig {
        chain_id: config.chain_id,
        genesis_hash: config.genesis_hash,
        start_block: config.storage.start_block,
    };
    let storage = StorageProvider::new(config.storage.clone())
        .and_then(|provider| provider.open_writer())
        .map_err(PrepareProjectionError::Storage)?;
    let reader = storage.reader.clone();
    match selector.as_ref() {
        Some(selector) => OffchainDataProjection::open_with_retention_selector(
            projection_config,
            reader.clone(),
            storage.writer.clone(),
            Arc::clone(selector),
        ),
        None => {
            OffchainDataProjection::open(projection_config, reader.clone(), storage.writer.clone())
        }
    }
    .map_err(PrepareProjectionError::Projection)?;
    storage
        .writer
        .verify_transaction_capability()
        .map_err(PrepareProjectionError::Storage)?;
    gauge!("outbe_projection_storage_write_capable", "backend" => config.storage.backend_name())
        .set(1.0);
    info!(
        backend = config.storage.backend_name(),
        "offchain storage opened"
    );
    let (overlay, projector) = open_logical_projection(projection_config, reader, selector)
        .map_err(PrepareProjectionError::Projection)?;
    Ok((storage, overlay, projector))
}

pub(super) fn open_logical_projection(
    projection_config: ProjectionConfig,
    durable_reader: StorageReaderHandle,
    selector: Option<Arc<dyn TributeRetentionSelector>>,
) -> Result<
    (Arc<PendingOverlayStorage>, OffchainDataProjection),
    outbe_offchain_data::ProjectionError,
> {
    let overlay = Arc::new(PendingOverlayStorage::new(durable_reader));
    let projector = match selector {
        Some(selector) => OffchainDataProjection::open_with_retention_selector(
            projection_config,
            overlay.clone(),
            overlay.clone(),
            selector,
        )?,
        None => OffchainDataProjection::open(projection_config, overlay.clone(), overlay.clone())?,
    };
    Ok((overlay, projector))
}

/// Validates a persisted checkpoint against canonical Reth state during ExEx initialization.
pub fn validate_offchain_data_checkpoint<P>(
    prepared: PreparedOffchainDataProjection,
    canonical_hashes: &P,
) -> eyre::Result<ReadyOffchainDataProjection>
where
    P: BlockHashReader + BlockIdReader,
{
    let projector = prepared.projector;
    let projection_config = ProjectionConfig {
        chain_id: projector.state().chain_id,
        genesis_hash: projector.state().genesis_hash,
        start_block: projector.state().start_block,
    };
    let overlay = prepared.overlay;
    let reader: StorageReaderHandle = overlay.clone();
    let writer = prepared.storage.writer;
    let readiness_publisher = prepared.readiness_publisher;
    let runtime_failure_sender = prepared.runtime_failure_sender;
    let runtime_failure_receiver = prepared.runtime_failure_receiver;
    let retention_fence = prepared.retention_fence;
    let writer_lease = prepared.storage.ownership;
    let local_finalized = canonical_hashes
        .finalized_block_num_hash()
        .wrap_err("read local Reth finalized checkpoint for offchain-data validation")?
        .map(|block| FinalizedTarget::new(block.number, block.hash));
    if let Some(checkpoint) = projector.state().checkpoint {
        let reconciled = require_finalized_checkpoint(checkpoint, local_finalized)?;
        match canonical_hashes
            .block_hash(checkpoint.block_number)
            .wrap_err("read canonical Reth hash for offchain-data checkpoint validation")?
        {
            Some(canonical_hash) if canonical_hash == checkpoint.block_hash => {}
            Some(canonical_hash) => return Err(eyre::eyre!(
                "offchain-data offchain storage checkpoint identity mismatch at block {}: stored {}, canonical {}",
                checkpoint.block_number,
                checkpoint.block_hash,
                canonical_hash
            )),
            None => return Err(eyre::eyre!(
                "canonical block {} for Mongo checkpoint {} is unavailable locally",
                checkpoint.block_number,
                checkpoint.block_hash
            )),
        }
        if reconciled.is_some_and(|target| checkpoint.block_number == target.number) {
            publish_status(
                &readiness_publisher,
                ProjectionStatus::Ready { checkpoint },
                local_finalized,
            );
        } else {
            publish_status(
                &readiness_publisher,
                ProjectionStatus::CatchingUp {
                    checkpoint: Some(checkpoint),
                },
                local_finalized,
            );
        }
    } else {
        let target = local_finalized.map(|block| FinalizedTarget::new(block.number, block.hash));
        let status = match target {
            Some(target) if target.number == 0 && target.hash == projection_config.genesis_hash => {
                ProjectionStatus::Ready {
                    checkpoint: ProjectionCheckpoint {
                        block_number: 0,
                        block_hash: projection_config.genesis_hash,
                    },
                }
            }
            _ => ProjectionStatus::CatchingUp { checkpoint: None },
        };
        publish_status(&readiness_publisher, status, target);
    }
    let projection_state = projector.state();
    info!(
        chain_id = projection_state.chain_id,
        genesis_hash = %projection_state.genesis_hash,
        start_block = projection_state.start_block,
        "finalized offchain-data projection ready"
    );
    Ok(ReadyOffchainDataProjection {
        projector,
        readiness_publisher,
        projection_config,
        _reader: reader,
        overlay,
        writer,
        writer_lease,
        runtime_failure_sender,
        runtime_failure_receiver,
        retention_fence,
    })
}

pub(super) fn require_finalized_checkpoint(
    checkpoint: ProjectionCheckpoint,
    local_finalized: Option<FinalizedTarget>,
) -> eyre::Result<Option<FinalizedTarget>> {
    let Some(local_finalized) = local_finalized else {
        return Ok(None);
    };
    if checkpoint.block_number > local_finalized.number {
        return Ok(None);
    }
    if checkpoint.block_number == local_finalized.number
        && checkpoint.block_hash != local_finalized.hash
    {
        bail!(
            "offchain-data offchain storage checkpoint {} ({}) does not match local Reth finalized {} ({})",
            checkpoint.block_number,
            checkpoint.block_hash,
            local_finalized.number,
            local_finalized.hash
        );
    }
    Ok(Some(local_finalized))
}
