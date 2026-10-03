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
        // Register the block and barrier before releasing the digest. Relay
        // starts persistence after dissemination; certify awaits its completion.
        if !self.publication.stage(round, block) {
            return Ok(ProposeOutcome::RoundAlreadyProposed);
        }
        Ok(ProposeOutcome::Proposed(digest))
    }
}
