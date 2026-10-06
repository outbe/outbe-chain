//! Execution validity is decided by the executor without selecting HEAD.
use super::{ApplicationShared, ExecutionReadBudget, ResolvedVerifyBlocks, VerifyRequest};
use crate::executor::ingress::{VerificationOutcome, VerificationRequest};
use std::sync::Arc;

impl ApplicationShared {
    pub(super) async fn verify_block_execution(
        &self,
        request: &VerifyRequest,
        resolved: &ResolvedVerifyBlocks,
        execution_read_budget: &ExecutionReadBudget,
    ) -> eyre::Result<VerificationOutcome> {
        let outcome = self
            .executor_mailbox
            .verify_block(VerificationRequest {
                round: request.context.round,
                block: Arc::new(resolved.block.clone()),
                parent: resolved
                    .parent_block
                    .as_ref()
                    .map(|block| Arc::new(block.clone())),
                epoch_fence: self.epoch_fence.clone(),
                execution_read_budget: execution_read_budget.clone(),
            })?
            .await;
        Ok(outcome.unwrap_or(VerificationOutcome::Unavailable))
    }
}
