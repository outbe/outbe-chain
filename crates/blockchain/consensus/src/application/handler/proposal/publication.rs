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
    pub(in crate::application::handler) async fn publish_built_proposal(
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
            // An unavailable acknowledgement or an aborted sync cannot authorize
            // publication. Actual storage failures retain marshal's fatal policy.
            return Err(eyre::eyre!(
                "marshal did not durably acknowledge proposal {digest:?} at {round}"
            ));
        }
        Ok(ProposeOutcome::Proposed(digest))
    }
}
