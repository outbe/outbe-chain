//! Execution validity is decided by the executor without selecting HEAD.
use super::{ApplicationShared, ExecutionReadBudget, ResolvedVerifyBlocks, VerifyRequest};
use crate::executor::ingress::{VerificationOutcome, VerificationRequest};
use crate::metrics::verify::record_block_verify_decision_ms;
use std::{sync::Arc, time::Instant};

impl ApplicationShared {
    pub(super) async fn verify_block_execution(
        &self,
        request: &VerifyRequest,
        resolved: &ResolvedVerifyBlocks,
        execution_read_budget: &ExecutionReadBudget,
    ) -> eyre::Result<VerificationOutcome> {
        let started_at = Instant::now();
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
        record_block_verify_decision_ms(started_at.elapsed());
        Ok(outcome.unwrap_or(VerificationOutcome::Unavailable))
    }
}
