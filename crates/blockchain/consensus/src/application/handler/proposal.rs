use super::parent_round;
use super::proposal_timestamp_millis;
use super::wait_for_projected_parent;
use super::ApplicationShared;
use super::EngineHandle;
use super::ParentProjectionGate;
use super::ParentProofLookup;

use crate::application::epoch_boundary;

use crate::block::ConsensusBlock;

use crate::config::PROPOSE_RESOLUTION_TIMEOUT;

use crate::digest::Digest;

use crate::dkg_manager::BoundaryRequirement;
use crate::dkg_manager::ProposalForfeit;

use crate::finalization::parent_cert_store::CertifiedParentProofKey;

use crate::finalization::state::FinalizationViewAccess;

use alloy_primitives::Bytes;
use alloy_primitives::B256;
use alloy_rpc_types_engine::PayloadId;
use commonware_consensus::types::Height;
use commonware_consensus::types::Round;
use commonware_consensus::types::View;

use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::projection::ExecutionReadBudget;
use outbe_primitives::projection::ProjectionCheckpoint;

use outbe_primitives::reshare_artifact::encode_outbe_block_artifacts;

use outbe_primitives::reshare_artifact::OutbeBlockArtifacts;

use outbe_primitives::OutbeExecutionData;
use outbe_primitives::OutbePayloadAttributes;

use reth_node_builder::BuiltPayload as _;

use std::sync::Arc;

use tracing::debug;

use tracing::warn;

mod attributes;
mod build;
mod parent;
mod publication;

pub(super) struct ProposalRequest {
    pub context: super::ingress::SimplexContext,
    pub propose_start: std::time::SystemTime,
    pub execution_read_budget: ExecutionReadBudget,
    pub payload_trace: ProposalPayloadTrace,
}
pub(super) struct BlockBuildRequest {
    pub round: Round,
    pub parent: ProposalParent,
    pub propose_start: std::time::SystemTime,
    pub execution_read_budget: ExecutionReadBudget,
    pub payload_trace: ProposalPayloadTrace,
}
pub(super) struct ProposalParent {
    pub(super) height: Height,
    pub(super) digest: Digest,
    pub(super) block: Option<ConsensusBlock>,
    pub(super) proof_key: Option<CertifiedParentProofKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProposeOutcome {
    Proposed(Digest),
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
    ProjectionUnavailable,
    ExecutionUnavailable,
    RoundAlreadyProposed,
}

impl ProposeOutcome {
    pub(super) fn completion_message(self) -> &'static str {
        match self {
            Self::Proposed(_) => "proposal task completed with a built candidate",
            Self::ParentProofUnavailable => {
                "proposal task completed without response: exact parent proof unavailable"
            }
            Self::EpochStale => "proposal task completed without response for stale epoch work",
            Self::BoundaryUnavailable => {
                "proposal task completed without response: DKG boundary requirement unavailable"
            }
            Self::ProjectionUnavailable => {
                "proposal task completed without response: exact parent is not projected"
            }
            Self::ExecutionUnavailable => {
                "proposal task completed without response: candidate execution is not valid"
            }
            Self::RoundAlreadyProposed => {
                "proposal task completed without response: round already has a candidate"
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ProposalPayloadTrace {
    payload_id: Arc<std::sync::OnceLock<PayloadId>>,
}

impl ProposalPayloadTrace {
    pub(super) fn record(&self, payload_id: PayloadId) {
        let _ = self.payload_id.set(payload_id);
    }

    pub(super) fn payload_id(&self) -> Option<PayloadId> {
        self.payload_id.get().copied()
    }
}

// `Built` carries the full `ConsensusBlock`. The other variants are unit. This is
// an internal result returned once per propose and consumed immediately. Boxing
// the block would only add an allocation on the hot proposer path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(super) enum BuildBlockOutcome {
    Built(Digest, ConsensusBlock),
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum BuiltCandidatePreparationError {
    #[error("locally built payload was not accepted by execution: {0}")]
    Execution(String),
}

/// Import and validate a locally built candidate before advertising it.
pub(super) async fn prepare_built_candidate(
    engine: &EngineHandle,
    block: &ConsensusBlock,
    execution_read_budget: ExecutionReadBudget,
) -> Result<(), BuiltCandidatePreparationError> {
    let execution_data = OutbeExecutionData::new(std::sync::Arc::new(block.clone().into_inner()))
        .with_execution_read_budget(execution_read_budget);
    let status = engine
        .new_payload(execution_data)
        .await
        .map_err(|error| BuiltCandidatePreparationError::Execution(error.to_string()))?;
    if !status.is_valid() {
        return Err(BuiltCandidatePreparationError::Execution(format!(
            "{status:?}"
        )));
    }
    Ok(())
}

impl ApplicationShared {
    /// Handle propose request strict wait.
    ///
    /// 1. Resolve parent block (local cache or marshal)
    /// 2. Send parent to execution layer via new_payload (ensure Reth knows it)
    /// 3. Canonicalize parent as head (FCU)
    /// 4. Build next block
    pub(super) async fn handle_propose(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: ProposalRequest,
    ) -> eyre::Result<ProposeOutcome> {
        let ProposalRequest {
            context,
            propose_start,
            execution_read_budget,
            payload_trace,
        } = request;
        let round = context.round;
        if self.publication.contains_round(round)
            || self.marshal_mailbox.get_verified(round).await.is_some()
        {
            // A restarted view must not build a conflicting candidate under a
            // changed context. The original candidate remains recoverable.
            return Ok(ProposeOutcome::RoundAlreadyProposed);
        }
        let Some(parent) = self.resolve_proposal_parent(clock, context).await? else {
            return Ok(ProposeOutcome::ParentProofUnavailable);
        };
        let next_block_number = parent.height.get().saturating_add(1);
        if let Some(outcome) = self.proposal_parent_gate(round, &parent).await? {
            return Ok(outcome);
        }
        self.executor_mailbox
            .report_pending_parent(crate::executor::ingress::PendingParent {
                round,
                digest: parent.digest,
                height: parent.height,
                block: parent.block.as_ref().map(|block| Arc::new(block.clone())),
                epoch_fence: self.epoch_fence.clone(),
            })?;
        self.import_proposal_parent(&parent, execution_read_budget.clone())
            .await?;
        self.vrf_safety
            .ensure_block_allowed(next_block_number)
            .map_err(|error| eyre::eyre!("refusing proposal above VRF expiry: {error}"))?;

        // Steps 3+4: Canonicalize parent as head and build next block.
        // Uses FCU-based flow. canonicalize_and_build sends FCU with payload
        // attributes. Then the engine starts to build a payload on the correct
        // canonical state with access to the txpool.
        let candidate_execution_budget = execution_read_budget.clone();
        let outcome = self
            .build_block(
                clock,
                BlockBuildRequest {
                    round,
                    parent,
                    propose_start,
                    execution_read_budget,
                    payload_trace,
                },
            )
            .await;
        self.finish_proposal_build(round, outcome, candidate_execution_budget)
            .await
    }

    async fn prepare_built_candidate(
        &self,
        block: &ConsensusBlock,
        execution_read_budget: ExecutionReadBudget,
    ) -> Result<(), BuiltCandidatePreparationError> {
        prepare_built_candidate(&self.engine, block, execution_read_budget).await
    }
}
