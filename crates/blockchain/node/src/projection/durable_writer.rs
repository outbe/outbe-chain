use super::FinalizedTarget;
use super::PROJECTION_RETRY_INTERVAL;
use alloy_primitives::B256;
use outbe_offchain_storage::AtomicWriteBatch;
use outbe_offchain_storage::PendingOverlayStorage;
use outbe_offchain_storage::StorageError;
use outbe_offchain_storage::StorageErrorKind;
use outbe_offchain_storage::StorageWriterHandle;
use std::sync::Arc;
use tracing::warn;

#[derive(Debug, thiserror::Error)]
#[error("offchain storage projection reconnect deadline expired for block {block_number} ({block_hash})")]
pub struct ProjectionWriteDeadlineError {
    pub(super) block_number: u64,
    pub(super) block_hash: B256,
    #[source]
    source: Option<StorageError>,
}

pub(super) struct DurableProjectionWrite {
    pub(super) checkpoint: FinalizedTarget,
    pub(super) batch: AtomicWriteBatch,
    pub(super) overlay_ack: Option<(Arc<PendingOverlayStorage>, u64)>,
}

pub(super) fn apply_durable_projection_write_before(
    writer: &StorageWriterHandle,
    write: &DurableProjectionWrite,
    deadline: std::time::Instant,
) -> eyre::Result<()> {
    let mut last_unavailable = None;
    loop {
        if std::time::Instant::now() >= deadline {
            if let Some(source) = last_unavailable.take() {
                return Err(ProjectionWriteDeadlineError {
                    block_number: write.checkpoint.number,
                    block_hash: write.checkpoint.hash,
                    source: Some(source),
                }
                .into());
            }
        }
        match writer.apply_atomic(&write.batch) {
            Ok(()) => {
                if std::time::Instant::now() >= deadline {
                    return Err(ProjectionWriteDeadlineError {
                        block_number: write.checkpoint.number,
                        block_hash: write.checkpoint.hash,
                        source: None,
                    }
                    .into());
                }
                if let Some((overlay, generation)) = &write.overlay_ack {
                    overlay.acknowledge(*generation);
                }
                return Ok(());
            }
            Err(error)
                if error.kind() == StorageErrorKind::Unavailable
                    && std::time::Instant::now() < deadline =>
            {
                warn!(
                    %error,
                    block_number = write.checkpoint.number,
                    block_hash = %write.checkpoint.hash,
                    "offchain storage projection write failed; retrying exact atomic batch"
                );
                std::thread::sleep(
                    PROJECTION_RETRY_INTERVAL
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
                last_unavailable = Some(error);
            }
            Err(source) if source.kind() == StorageErrorKind::Unavailable => {
                return Err(ProjectionWriteDeadlineError {
                    block_number: write.checkpoint.number,
                    block_hash: write.checkpoint.hash,
                    source: Some(source),
                }
                .into());
            }
            Err(error) => return Err(error.into()),
        }
    }
}
