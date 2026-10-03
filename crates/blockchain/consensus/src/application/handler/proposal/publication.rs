use super::*;
impl ApplicationShared {
    pub(super) async fn finish_proposal_build(
        &self,
        round: Round,
        outcome: eyre::Result<BuildBlockOutcome>,
        candidate_execution_budget: ExecutionReadBudget,
    ) -> eyre::Result<ProposeOutcome> {
        match outcome {
            Ok(BuildBlockOutcome::Built(digest, block)) => {
                self.publish_built_proposal(round, (digest, block), candidate_execution_budget)
                    .await
            }
            Ok(BuildBlockOutcome::ParentProofUnavailable) => {
                Ok(ProposeOutcome::ParentProofUnavailable)
            }
            Ok(BuildBlockOutcome::EpochStale) => Ok(ProposeOutcome::EpochStale),
            Ok(BuildBlockOutcome::BoundaryUnavailable) => Ok(ProposeOutcome::BoundaryUnavailable),
            Err(e) => Err(eyre::eyre!("failed to build block for proposal: {e}")),
        }
    }
    pub(super) async fn publish_built_proposal(
        &self,
        round: Round,
        candidate: (Digest, ConsensusBlock),
        candidate_execution_budget: ExecutionReadBudget,
    ) -> eyre::Result<ProposeOutcome> {
        let (digest, block) = candidate;
        if let Err(error) = self
            .prepare_built_candidate(&block, candidate_execution_budget)
            .await
        {
            warn!(
                %round,
                digest = %digest.0,
                %error,
                "withholding proposal because candidate execution is not valid"
            );
            return Ok(ProposeOutcome::ExecutionUnavailable);
        }
        // Persist before returning the proposal. The new `proposed` API also
        // broadcasts; `verified` retains our separate durable-cache and
        // Relay::broadcast paths, so a dropped push remains recoverable by pull.
        let durable = self.marshal_mailbox.verified(round, block).await;
        if !durable {
            // `verified()` returns false only when the marshal actor's ack
            // channel is closed - i.e. marshal is gone/shutting down. The
            // block is then NOT durably cached (not servable on pull, not
            // stashed for `forward`), so this proposal cannot be resolved by
            // verifiers (bp-1 pull-recovery does not help - nothing to serve).
            // Surface it loudly rather than silently treating the proposal as
            // durable. A persistent marshal failure is the supervisor's
            // concern: the marshal handle is monitored (SSA-8) and a dead
            // marshal fails the node fast.
            warn!(
                %round,
                digest = %digest.0,
                "marshal did not acknowledge proposed block (mailbox closed); \
                 proposal is not durably cached"
            );
        }
        Ok(ProposeOutcome::Proposed(digest))
    }
}
