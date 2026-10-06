use super::super::apply_durable_projection_write_before;
use super::super::DurableProjectionWrite;
use super::super::FinalizedTarget;
use super::super::PROJECTION_RECOVERY_DEADLINE;
use outbe_offchain_storage::StorageWriterHandle;
use std::time::Duration;

#[cfg(test)]
pub(in super::super) fn apply_durable_projection_write_until(
    writer: &StorageWriterHandle,
    write: &DurableProjectionWrite,
    deadline: Duration,
) -> eyre::Result<()> {
    apply_durable_projection_write_before(writer, write, std::time::Instant::now() + deadline)
}

#[cfg(test)]
pub(in super::super) fn run_durable_projection_writer(
    writer: StorageWriterHandle,
    mut writes: tokio::sync::mpsc::UnboundedReceiver<DurableProjectionWrite>,
    durable_checkpoint_tx: tokio::sync::mpsc::UnboundedSender<FinalizedTarget>,
    durable_error_tx: tokio::sync::mpsc::UnboundedSender<eyre::Report>,
) {
    while let Some(write) = writes.blocking_recv() {
        if let Err(error) = apply_durable_projection_write_before(
            &writer,
            &write,
            std::time::Instant::now() + PROJECTION_RECOVERY_DEADLINE,
        ) {
            let _ = durable_error_tx.send(error);
            return;
        }
        if durable_checkpoint_tx.send(write.checkpoint).is_err() {
            return;
        }
    }
}
