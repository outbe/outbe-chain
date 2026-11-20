//! Verification stages preserve the distinction between rejection and local unavailability.
use super::ApplicationShared;
use crate::{
    application::{
        ingress::SimplexContext,
        validation::{
            validate_rewards_beneficiary, validate_system_tx_leader_binding_for_activation,
            SystemTxLeaderValidationContext,
        },
    },
    block::ConsensusBlock,
    committee_provider::CommitteeProvider,
    digest::Digest,
    dkg_manager::{AncestryReader, ArtifactAdmissionError, BoundaryRequirement},
    finalization::util::extract_header_artifact_from_block,
    hybrid::HybridSchemeProvider,
    metrics::verify::{
        record_candidate_timestamp_ahead_ms, VerifyInFlightGuard, VoteWithheldReason,
        WithheldVoteRecord,
    },
};
use alloy_primitives::Address;
use commonware_consensus::types::Round;
use commonware_cryptography::bls12381::{primitives::variant::MinSig, PublicKey};
use commonware_utils::channel::oneshot;
use outbe_primitives::projection::ExecutionReadBudget;
use tracing::warn;

mod execution;
mod prechecks;
mod resolution;
#[cfg(test)]
mod tests;
mod verdict;

/// Owned inputs and cancellation budget for one spawned verification task.
pub(super) struct VerifyTask {
    pub(super) context: SimplexContext,
    pub(super) payload_digest: Digest,
    pub(super) response: oneshot::Sender<bool>,
    pub(super) execution_read_budget: ExecutionReadBudget,
}

/// Request identity stays in the canonical Simplex context throughout every stage.
struct VerifyRequest {
    context: SimplexContext,
    payload_digest: Digest,
}

impl VerifyRequest {
    fn parent_digest(&self) -> Digest {
        self.context.parent.1
    }
}

struct ResolvedVerifyBlocks {
    block: ConsensusBlock,
    parent_block: Option<ConsensusBlock>,
}

/// Whether the local node validates live proposals. A share-less verifier (a TEE
/// full-node with no proposer EVM address) follows FINALIZED blocks only. It skips
/// the leader-binding / DKG-boundary checks. These checks are
/// polynomial/DKG-view-dependent and would diverge on a verifier's stale
/// post-rotation state. This enum replaces a boolean-blind `is_verifier` flag so
/// the role choice is explicit in the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValidatorRole {
    Signer,
    VerifierOnly,
}

impl ValidatorRole {
    /// A node with no proposer EVM address is a share-less verifier-only follower.
    fn from_proposer_evm_address(proposer_evm_address: Option<Address>) -> Self {
        match proposer_evm_address {
            Some(_) => Self::Signer,
            None => Self::VerifierOnly,
        }
    }
}

/// Facts carried by one proposal being checked for header-artifact admission.
pub(super) struct HeaderArtifactRequest<'a> {
    pub(super) block: &'a ConsensusBlock,
    pub(super) parent_block: Option<&'a ConsensusBlock>,
    pub(super) round: Round,
    pub(super) proposer: &'a PublicKey,
    pub(super) role: ValidatorRole,
}

/// Existing chain policy and read-only admission dependencies, borrowed for the request.
pub(super) struct HeaderArtifactValidationDeps<'a, A: AncestryReader> {
    pub(super) chain_id: u64,
    pub(super) leader_policy: outbe_primitives::system_tx::binding::SystemTxLeaderPolicy,
    pub(super) certificate_scheme_provider: &'a HybridSchemeProvider<MinSig>,
    pub(super) committee_provider: &'a CommitteeProvider,
    pub(super) dkg_manager: &'a crate::dkg_manager::Mailbox,
    pub(super) ancestry: &'a A,
}

pub(super) async fn validate_header_consensus_artifacts_for_activation(
    request: HeaderArtifactRequest<'_>,
    deps: HeaderArtifactValidationDeps<'_, impl AncestryReader>,
) -> Result<(), ArtifactAdmissionError> {
    let HeaderArtifactRequest {
        block,
        parent_block,
        round,
        proposer,
        role,
    } = request;
    let HeaderArtifactValidationDeps {
        chain_id,
        leader_policy,
        certificate_scheme_provider,
        committee_provider,
        dkg_manager,
        ancestry,
    } = deps;
    // Finalized-follower rule: a share-less verifier (a TEE full-node, no
    // `proposer_evm_address`) does NOT validate live proposals. It follows
    // FINALIZED blocks. The reporter verifies their threshold certificate against
    // the GROUP public key (preserved across reshares). The leader-binding and
    // DKG-boundary checks below are polynomial/DKG-view-dependent and would
    // diverge on a verifier's stale post-rotation state, so this function skips
    // them. Consensus safety for the follower comes from the finalization
    // certificate, not from re-deriving the live proposal's leader. The verifier
    // never votes (`me()` is None), so accepting the proposal here cannot affect
    // the committee's quorum.
    if role == ValidatorRole::VerifierOnly {
        return Ok(());
    }
    validate_rewards_beneficiary(block)?;
    validate_system_tx_leader_binding_for_activation(
        block,
        SystemTxLeaderValidationContext {
            round,
            proposer,
            chain_id,
            leader_policy,
            certificate_scheme_provider,
            committee_provider,
        },
    )?;

    let artifact = extract_header_artifact_from_block(block)?;
    let requirement = dkg_manager
        .admit_header_artifact(parent_block, round.epoch(), artifact.as_ref(), ancestry)
        .await?;
    if requirement == BoundaryRequirement::AlreadyCommitted {
        crate::metrics::record_dkg_boundary_requirement(
            crate::metrics::DkgBoundaryDecision::AlreadyCommitted,
        );
    }
    Ok(())
}

impl ApplicationShared {
    /// One response owner races the entire decision against consensus cancellation.
    pub(super) async fn handle_verify(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        task: VerifyTask,
    ) {
        let _in_flight = VerifyInFlightGuard::new();
        let withheld_vote = WithheldVoteRecord::default();
        let VerifyTask {
            context,
            payload_digest,
            mut response,
            execution_read_budget,
        } = task;
        let request = VerifyRequest {
            context,
            payload_digest,
        };
        let decide = async {
            match self
                .decide_verification(clock, &request, &execution_read_budget, &withheld_vote)
                .await
            {
                Ok(crate::executor::ingress::VerificationOutcome::Unavailable) => {
                    withheld_vote.record(VoteWithheldReason::Unavailable);
                    std::future::pending().await
                }
                Err(error) => {
                    warn!(%error, "local verification unavailable; withholding vote until cancellation");
                    withheld_vote.record(VoteWithheldReason::VerifyError);
                    std::future::pending().await
                }
                Ok(outcome) => outcome,
            }
        };
        let outcome =
            match futures::future::select(Box::pin(response.closed()), Box::pin(decide)).await {
                futures::future::Either::Left(_) => {
                    withheld_vote.record(VoteWithheldReason::ViewCancelled);
                    execution_read_budget.cancel();
                    return;
                }
                futures::future::Either::Right((outcome, _)) => outcome,
            };
        self.publish_verify_verdict(&request, response, outcome, &withheld_vote);
    }

    async fn decide_verification(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: &VerifyRequest,
        execution_read_budget: &ExecutionReadBudget,
        withheld_vote: &WithheldVoteRecord,
    ) -> eyre::Result<crate::executor::ingress::VerificationOutcome> {
        use crate::executor::ingress::{PendingParent, VerificationOutcome};
        let round = request.context.round;
        if self.epoch_fence.check(round, 0).is_err() {
            return Ok(VerificationOutcome::Invalid);
        }
        let Some(resolved) = self
            .resolve_verify_blocks(clock, request, withheld_vote)
            .await?
        else {
            return Ok(VerificationOutcome::Invalid);
        };
        let candidate_ts_ms = resolved.block.timestamp_millis();
        let role = ValidatorRole::from_proposer_evm_address(self.proposer_evm_address);
        self.maybe_record_candidate_timestamp_ahead(role, candidate_ts_ms);
        let parent = resolved
            .parent_block
            .as_ref()
            .map(|block| std::sync::Arc::new(block.clone()));
        self.executor_mailbox.report_pending_parent(PendingParent {
            round,
            digest: request.parent_digest(),
            height: commonware_consensus::types::Height::new(
                parent.as_ref().map_or(0, |block| block.number()),
            ),
            block: parent,
            epoch_fence: self.epoch_fence.clone(),
        })?;
        if !self
            .validate_verify_blocks(clock, request, &resolved)
            .await?
        {
            return Ok(VerificationOutcome::Invalid);
        }
        let outcome = self
            .verify_block_execution(request, &resolved, execution_read_budget)
            .await?;
        if matches!(outcome, VerificationOutcome::Valid) && role == ValidatorRole::Signer {
            self.wait_until_candidate_timestamp(clock, candidate_ts_ms)
                .await?;
        }
        Ok(
            if self
                .epoch_fence
                .check(round, resolved.block.number())
                .is_ok()
            {
                outcome
            } else {
                withheld_vote.record(VoteWithheldReason::EpochFence);
                VerificationOutcome::Unavailable
            },
        )
    }

    fn maybe_record_candidate_timestamp_ahead(&self, role: ValidatorRole, candidate_ts_ms: u64) {
        if role != ValidatorRole::Signer {
            return;
        }
        let Ok(local_now_ms) = self.unix_time_source.now_millis() else {
            return;
        };
        if candidate_ts_ms > local_now_ms {
            record_candidate_timestamp_ahead_ms(candidate_ts_ms - local_now_ms);
        }
    }

    async fn wait_until_candidate_timestamp(
        &self,
        clock: &impl commonware_runtime::Clock,
        candidate_ts_ms: u64,
    ) -> eyre::Result<()> {
        loop {
            let now_ms = self.unix_time_source.now_millis()?;
            if now_ms >= candidate_ts_ms {
                return Ok(());
            }
            clock
                .sleep(std::time::Duration::from_millis(candidate_ts_ms - now_ms))
                .await;
        }
    }
}
