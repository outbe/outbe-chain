//! Publish a live terminal verdict before best-effort verified-block side effects.
use super::{ApplicationShared, ConsensusBlock, PayloadVerification, VerifyRequest};
use crate::finalization::state::FinalizationViewAccess;
use commonware_consensus::types::Height;
use commonware_utils::channel::oneshot;
use tracing::debug;

impl ApplicationShared {
    pub(super) async fn publish_verify_verdict(
        &self,
        request: &VerifyRequest,
        block: &ConsensusBlock,
        response: oneshot::Sender<bool>,
        outcome: PayloadVerification,
    ) -> eyre::Result<()> {
        let block_saw_syncing = match outcome {
            PayloadVerification::ChannelClosed => return Ok(()),
            PayloadVerification::Invalid => {
                let _ = response.send(false);
                return Ok(());
            }
            PayloadVerification::Valid { saw_syncing } => saw_syncing,
        };
        let round = request.context.round;
        let payload_digest = request.payload_digest;
        let block_height = Height::new(block.number());
        if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
            debug!(
                %round,
                digest = %payload_digest.0,
                block_number = block.number(),
                ?rejection,
                "skipping verify block side effects after stale epoch transition"
            );
            return Ok(());
        }
        if response.is_closed() {
            debug!(
                digest = %payload_digest.0,
                "verify response channel closed before execution-valid side effects"
            );
            return Ok(());
        }
        if response.send(true).is_err() {
            debug!(
                digest = %payload_digest.0,
                "verify response receiver dropped before execution-valid side effects"
            );
            return Ok(());
        }
        if block_saw_syncing {
            debug!(
                digest = %payload_digest.0,
                "skipping verify block side effects after pending/cancelable execution validation"
            );
            return Ok(());
        }
        let _ = self.marshal_mailbox.verified(round, block.clone()).await;
        let _ = self
            .executor_mailbox
            .canonicalize_head(block_height, payload_digest)
            .await;
        self.finalization_view
            .advance_timestamp_floor(block.timestamp_millis());

        Ok(())
    }
}
