use super::FinalizedTarget;
use super::PROJECTION_RETRY_INTERVAL;
use alloy_primitives::B256;
use outbe_offchain_storage::AtomicWriteBatch;
use outbe_offchain_storage::StorageError;
use outbe_offchain_storage::StorageErrorKind;
use outbe_offchain_storage::StorageWriterHandle;
use outbe_offchain_storage::{PendingDurableReceipt, PendingOverlayStorage, PendingWrite};
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
    batch: ProjectionBatch,
}

enum ProjectionBatch {
    Direct(AtomicWriteBatch),
    Pending(PendingWrite),
}

impl DurableProjectionWrite {
    pub(super) fn direct(checkpoint: FinalizedTarget, batch: AtomicWriteBatch) -> Self {
        Self {
            checkpoint,
            batch: ProjectionBatch::Direct(batch),
        }
    }
    pub(super) fn pending(checkpoint: FinalizedTarget, pending: PendingWrite) -> Self {
        Self {
            checkpoint,
            batch: ProjectionBatch::Pending(pending),
        }
    }
    pub(super) fn prepare(
        projector: &mut outbe_offchain_data::OffchainDataProjection,
        prepared: outbe_offchain_data::PreparedBlock,
        overlay: &PendingOverlayStorage,
    ) -> Result<(outbe_offchain_data::ProjectionOutcome, Self), outbe_offchain_data::ProjectionError>
    {
        use outbe_offchain_data::ProjectionOutcome;
        let (outcome, pending) =
            projector.apply_prepared_with(prepared, |batch| overlay.stage(batch))?;
        let batch = pending
            .map(ProjectionBatch::Pending)
            .unwrap_or_else(|| ProjectionBatch::Direct(AtomicWriteBatch::new()));
        let checkpoint = match &outcome {
            ProjectionOutcome::Applied { checkpoint, .. }
            | ProjectionOutcome::AlreadyApplied(checkpoint) => checkpoint,
        };
        let target = FinalizedTarget::new(checkpoint.block_number, checkpoint.block_hash);
        let write = match batch {
            ProjectionBatch::Direct(batch) => Self::direct(target, batch),
            ProjectionBatch::Pending(pending) => Self::pending(target, pending),
        };
        Ok((outcome, write))
    }
    fn persist(
        &self,
        writer: &StorageWriterHandle,
    ) -> Result<Option<PendingDurableReceipt<'_>>, StorageError> {
        match &self.batch {
            ProjectionBatch::Direct(batch) => writer.apply_atomic(batch).map(|_| None),
            ProjectionBatch::Pending(pending) => pending.persist().map(Some),
        }
    }
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
        match write.persist(writer) {
            Ok(receipt) => {
                if std::time::Instant::now() >= deadline {
                    return Err(ProjectionWriteDeadlineError {
                        block_number: write.checkpoint.number,
                        block_hash: write.checkpoint.hash,
                        source: None,
                    }
                    .into());
                }
                if let Some(receipt) = receipt {
                    receipt.acknowledge();
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
