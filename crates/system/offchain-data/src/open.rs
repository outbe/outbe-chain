//! Open durable projection state before finalized block processing.

use super::{
    read_projection_state, state::contains_unmanaged_data, state::state_batch,
    OffchainDataProjection, ProjectionConfig, ProjectionError, ProjectionState,
    TributeRetentionSelector, STORAGE_SCHEMA_VERSION,
};
use outbe_offchain_storage::{StorageReaderHandle, StorageWriterHandle};
use std::sync::Arc;

/// Opens a managed database or initializes an empty one.
pub fn open_projection(
    config: ProjectionConfig,
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
) -> Result<OffchainDataProjection, ProjectionError> {
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
    Ok(OffchainDataProjection {
        reader,
        writer,
        state,
        tribute_retention_selector: None,
        day_route: None,
        partition_retirement: false,
    })
}

/// Opens the projector with the node-owned active-pin selector.
pub fn open_projection_with_retention_selector(
    config: ProjectionConfig,
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
    selector: Arc<dyn TributeRetentionSelector>,
) -> Result<OffchainDataProjection, ProjectionError> {
    let mut projection = open_projection(config, reader, writer)?;
    projection.tribute_retention_selector = Some(selector);
    Ok(projection)
}
