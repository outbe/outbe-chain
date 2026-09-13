use super::super::apply_durable_projection_write_before;
use super::super::DurableProjectionWrite;
use super::super::FinalizedTarget;
use super::super::PROJECTION_RETRY_INTERVAL;
use outbe_offchain_storage::StorageWriterHandle;
use std::time::Duration;
use tracing::warn;

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
) {
    while let Some(write) = writes.blocking_recv() {
        apply_durable_projection_write(&writer, &write);
        if durable_checkpoint_tx.send(write.checkpoint).is_err() {
            return;
        }
    }
}

#[cfg(test)]
fn apply_durable_projection_write(writer: &StorageWriterHandle, write: &DurableProjectionWrite) {
    loop {
        match writer.apply_atomic(&write.batch) {
            Ok(()) => {
                if let Some((overlay, generation)) = &write.overlay_ack {
                    overlay.acknowledge(*generation);
                }
                return;
            }
            Err(error) => {
                warn!(
                    %error,
                    block_number = write.checkpoint.number,
                    block_hash = %write.checkpoint.hash,
                    "offchain storage projection write failed; retrying exact atomic batch"
                );
                std::thread::sleep(PROJECTION_RETRY_INTERVAL);
            }
        }
    }
}
