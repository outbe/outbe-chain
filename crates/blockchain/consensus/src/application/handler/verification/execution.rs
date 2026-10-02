//! Parent and proposed-block execution adapters; retries belong to the shared driver.
use super::{
    ApplicationShared, ConsensusBlock, ExecutionReadBudget, OutbeExecutionData,
    PayloadValidationRequest, PayloadVerification, ResolvedVerifyBlocks, VerifyRequest,
};
use crate::finalization::state::FinalizationViewAccess;
use commonware_consensus::types::Height;
use commonware_utils::channel::oneshot;
use tracing::{debug, warn};

impl ApplicationShared {
    pub(super) async fn verify_parent_execution(
        &self,
        clock: &impl commonware_runtime::Clock,
        request: &VerifyRequest,
        resolved: &ResolvedVerifyBlocks,
        response: &mut oneshot::Sender<bool>,
        execution_read_budget: &ExecutionReadBudget,
    ) -> eyre::Result<PayloadVerification> {
        let Some(parent_block) = &resolved.parent_block else {
            return Ok(PayloadVerification::Valid { saw_syncing: false });
        };
        let round = request.context.round;
        let parent_digest = request.parent_digest();
        let parent_height = Height::new(parent_block.number());
        let execution_data =
            OutbeExecutionData::new(std::sync::Arc::new(parent_block.clone().into_inner()))
                .with_execution_read_budget(execution_read_budget.clone());

        let parent_saw_syncing =
            if crate::test_faults::should_drop_new_payload_for_test(parent_height) {
                warn!(
                    height = %parent_height,
                    parent = %parent_digest.0,
                    "test-marshal-drop: skipping verify parent new_payload"
                );
                false
            } else {
                match self
                    .verify_payload_with_syncing_retry(
                        clock,
                        PayloadValidationRequest {
                            kind: "parent",
                            digest: parent_digest,
                            execution_data,
                            response,
                            execution_read_budget,
                        },
                    )
                    .await?
                {
                    PayloadVerification::ChannelClosed => {
                        return Ok(PayloadVerification::ChannelClosed)
                    }
                    PayloadVerification::Invalid => {
                        return Ok(PayloadVerification::Invalid);
                    }
                    PayloadVerification::Valid { saw_syncing } => saw_syncing,
                }
            };

        if response.is_closed() || parent_saw_syncing {
            debug!(
                parent = %parent_digest.0,
                parent_saw_syncing,
                "skipping verify parent side effects after pending/cancelable execution validation"
            );
        } else if let Err(rejection) = self.epoch_fence.check(round, resolved.block.number()) {
            debug!(
                %round,
                parent = %parent_digest.0,
                block_number = resolved.block.number(),
                ?rejection,
                "skipping verify parent side effects after stale epoch transition"
            );
        } else {
            if let Err(e) = self
                .executor_mailbox
                .canonicalize_head(parent_height, parent_digest)
                .await
            {
                return Err(eyre::eyre!(
                    "canonicalize_head failed for parent during verify: parent={} error={e}",
                    parent_digest.0
                ));
            }

            self.finalization_view
                .advance_timestamp_floor(parent_block.timestamp_millis());
        }
        Ok(PayloadVerification::Valid {
            saw_syncing: parent_saw_syncing,
        })
    }

    pub(super) async fn verify_block_execution(
        &self,
        clock: &impl commonware_runtime::Clock,
        request: &VerifyRequest,
        block: &ConsensusBlock,
        response: &mut oneshot::Sender<bool>,
        execution_read_budget: &ExecutionReadBudget,
    ) -> eyre::Result<PayloadVerification> {
        let payload_digest = request.payload_digest;
        let execution_data =
            OutbeExecutionData::new(std::sync::Arc::new(block.clone().into_inner()))
                .with_execution_read_budget(execution_read_budget.clone());
        let block_height = Height::new(block.number());
        if crate::test_faults::should_drop_new_payload_for_test(block_height) {
            warn!(height = %block_height, digest = %payload_digest.0, "test-marshal-drop: skipping verify block new_payload");
            Ok(PayloadVerification::Valid { saw_syncing: false })
        } else {
            self.verify_payload_with_syncing_retry(
                clock,
                PayloadValidationRequest {
                    kind: "block",
                    digest: payload_digest,
                    execution_data,
                    response,
                    execution_read_budget,
                },
            )
            .await
        }
    }
}
