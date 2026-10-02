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

mod parent;
mod publication;

pub(super) struct ProposalRequest {
    pub context: super::ingress::SimplexContext,
    pub propose_start: std::time::SystemTime,
    pub execution_read_budget: ExecutionReadBudget,
    pub payload_trace: ProposalPayloadTrace,
}
struct ProposalParent {
    height: Height,
    digest: Digest,
    block: Option<ConsensusBlock>,
    proof_key: Option<CertifiedParentProofKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProposeOutcome {
    Proposed(Digest),
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
    ProjectionUnavailable,
    ExecutionUnavailable,
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

// `Built` carries the full `ConsensusBlock`; the other variants are unit. This is
// an internal result returned once per propose and consumed immediately - boxing
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
        let Some(parent) = self.resolve_proposal_parent(clock, context).await? else {
            return Ok(ProposeOutcome::ParentProofUnavailable);
        };
        let next_block_number = parent.height.get().saturating_add(1);
        if let Some(outcome) = self.proposal_parent_gate(round, &parent).await? {
            return Ok(outcome);
        }
        self.import_proposal_parent(&parent, execution_read_budget.clone())
            .await?;
        self.vrf_safety
            .ensure_block_allowed(next_block_number)
            .map_err(|error| eyre::eyre!("refusing proposal above VRF expiry: {error}"))?;

        // Steps 3+4: Canonicalize parent as head and build next block.
        // Uses FCU-based flow: canonicalize_and_build sends
        // FCU with payload attributes so the engine starts building a payload
        // on the correct canonical state with access to the txpool.
        let candidate_execution_budget = execution_read_budget.clone();
        let outcome = self
            .build_block(
                clock,
                round,
                parent.height,
                parent.digest,
                parent.block.clone(),
                parent.proof_key,
                propose_start,
                execution_read_budget,
                payload_trace,
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

    /// Uses an FCU-based flow: sends fork_choice_updated with payload
    /// attributes through the executor actor, so the engine starts building
    /// a payload on the correct canonical state with txpool access.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn build_block(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        round: Round,
        parent_height: Height,
        parent_digest: Digest,
        parent_block: Option<ConsensusBlock>,
        parent_proof_key: Option<CertifiedParentProofKey>,
        propose_start: std::time::SystemTime,
        execution_read_budget: ExecutionReadBudget,
        payload_trace: ProposalPayloadTrace,
    ) -> eyre::Result<BuildBlockOutcome> {
        let next_block_number = parent_height.get().saturating_add(1);
        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before payload build"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        if crate::test_faults::should_drop_new_payload_for_test(Height::new(next_block_number)) {
            warn!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                "test-marshal-drop: skipping local proposal for dropped height"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        let now_millis = self.unix_time_source.now_millis()?;
        // Clamp the proposed timestamp into the deterministic two-sided drift band
        // `[parent + MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
        // parent + MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS]`. The lower bound
        // forces each block to advance chain time, denying a colluding leader
        // majority the `parent + 1 ms` timestamp freeze that stalls emission and
        // unbonding maturity; the upper bound (C-01) mirrors the validator check
        // in `outbe-node`'s `validate_against_parent_timestamp_millis`, so an
        // honest proposer never emits a block validators would reject as
        // over-drifted. Both bounds match the validator rule exactly, so the
        // clamp only ever shifts the timestamp into the accepted band - never out
        // of it. After a long stall `now_millis` may exceed the cap; the chain
        // self-heals, ratcheting time forward by at most one band per block until
        // it catches up to real time.
        //
        // Exception - the genesis child has no resolved consensus parent block,
        // so the band is meaningless and only monotonicity is enforced. The
        // validator side exempts the genesis parent (`parent.number() == 0`) from
        // both band bounds, so block 1 (~= genesis + now) always validates and no
        // unbonding-lock bypass is possible at the first block.
        let timestamp_millis = proposal_timestamp_millis(
            parent_block.as_ref(),
            now_millis,
            outbe_primitives::consensus::MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
            outbe_primitives::consensus::MIN_BLOCK_TIMESTAMP_ADVANCE_MILLIS,
        );
        let prev_randao = self.finalization_view.prev_randao();

        // build header.extra_data only from consensus header
        // artifacts that affect block hashing (DKG boundary/dealer-log).
        // Exact-parent finalization facts are carried in the begin-zone
        // Phase 1 system transaction body, not as a header attestation
        // backlog tag.
        //
        //(proposed_height == 1,
        // parent_height == 0) MUST carry `ConsensusHeaderArtifact::BoundaryOutcome`
        // in `extra_data`. If the epoch has no pending boundary for block 1,
        // the proposer forfeits the slot deterministically with the
        // `genesis_dkg_boundary_not_ready` reason - never propose block 1
        // without a real boundary artifact.
        let proposed_height = parent_height.get().saturating_add(1);
        let ancestry = super::ancestry::marshal_ancestry_reader(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            Some(round),
            PROPOSE_RESOLUTION_TIMEOUT,
            clock.child("ancestry"),
        );
        let plan = match self
            .dkg_manager
            .plan_header_artifact(
                parent_block.as_ref(),
                round.epoch(),
                proposed_height,
                &ancestry,
            )
            .await
        {
            Ok(plan) => plan,
            Err(ProposalForfeit::GenesisBoundaryNotReady) => {
                debug!(
                    %round,
                    proposed_height,
                    "block 1 proposal forfeited: DKG boundary artifact for epoch 0 not ready"
                );
                crate::metrics::record_genesis_dkg_boundary_not_ready_forfeit();
                crate::metrics::record_dkg_boundary_unavailable(
                    crate::metrics::DkgBoundaryUnavailableReason::GenesisBoundaryNotReady,
                );
                return Ok(BuildBlockOutcome::BoundaryUnavailable);
            }
            Err(ProposalForfeit::Boundary(error)) => {
                warn!(
                    %round,
                    proposed_height,
                    %error,
                    "block proposal forfeited: DKG boundary requirement unavailable"
                );
                if error.is_unavailable() {
                    crate::metrics::record_dkg_boundary_unavailable(
                        crate::metrics::DkgBoundaryUnavailableReason::AncestryUnavailable,
                    );
                }
                return Ok(BuildBlockOutcome::BoundaryUnavailable);
            }
        };
        crate::metrics::record_dkg_boundary_requirement(match plan.requirement {
            BoundaryRequirement::NoPending => crate::metrics::DkgBoundaryDecision::NoPending,
            BoundaryRequirement::AlreadyCommitted => {
                crate::metrics::DkgBoundaryDecision::AlreadyCommitted
            }
            BoundaryRequirement::MustEmit => crate::metrics::DkgBoundaryDecision::MustEmit,
        });
        let consensus_header_artifact = plan.artifact;
        #[cfg(all(
            feature = "e2e-byzantine-preannounce",
            feature = "test-protocol-overrides"
        ))]
        let consensus_header_artifact =
            super::byzantine_hook::override_artifact(plan.requirement, consensus_header_artifact);

        // Non-blocking direct-parent proof selection
        // (finalization first -> certified-notarization -> marshal-archive
        // recovery -> forfeit). The request budget does not gate this lookup -
        // the selector returns synchronously. On a selection-store
        // miss the None branch recovers the parent's finalization from marshal's
        // durable archive (, `recover_parent_proof_from_marshal`); only if
        // that also misses does the slot forfeit deterministically with the
        // parent-proof-unavailable metric.
        let parent_proof_record = match self
            .select_parent_proof_for_proposal(
                clock,
                round,
                parent_digest,
                parent_height,
                parent_proof_key,
            )
            .await
        {
            ParentProofLookup::NoProofNeeded => None,
            ParentProofLookup::Found(record) => Some(record),
            ParentProofLookup::Unavailable => return Ok(BuildBlockOutcome::ParentProofUnavailable),
        };
        // V2 wire-format swap landed. Build the V2
        // `CertifiedParentAccountingMetadata` directly from the proof record
        // via [`CertifiedParentProofRecord::to_v2_metadata`]. Both
        // finalization and certified-notarization records project into V2
        // metadata's `ParentProofSelector::select_direct_parent_proof`
        // is the upstream caller that decides which record (if any) to feed
        // into Phase 1.
        // The selector guarantees the chosen record's height resolves to
        // `parent_height` (Finalization validated to match; CertifiedNotarization
        // carries no height of its own and is resolved to the parent here).
        let parent_consensus_metadata = parent_proof_record
            .as_ref()
            .map(|record| record.to_v2_metadata(parent_height.get()));
        if parent_consensus_metadata.is_some() {
            crate::metrics::record_parent_cert_included();
        }

        // pack the in-window late-finalize credits this node has
        // locally observed for blocks `proposed_height - K ..= proposed_height - 1`.
        // Best-effort and process-local: every validator re-verifies each batch
        // (pre-exec FATAL) and re-derives the same artifact via header<->calldata
        // parity, so the contents never affect determinism - an empty store just
        // credits nobody. A poisoned lock degrades to no credits.
        let late_finalize_credits = match self.late_sig_store.lock() {
            Ok(store) => {
                let artifact = store.build_artifact(proposed_height);
                if artifact.batches.is_empty() {
                    None
                } else {
                    Some(artifact)
                }
            }
            Err(_) => None,
        };

        let header_extra_data =
            if consensus_header_artifact.is_none() && late_finalize_credits.is_none() {
                Bytes::new()
            } else {
                encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                    execution_summary: None,
                    consensus_header_artifact,
                    // The sub-second timestamp part is recomputed by the
                    // payload builder from `OutbeBlockExecutionCtx` and
                    // re-encoded into `extra_data` before sealing; we
                    // intentionally leave it at 0 here.
                    timestamp_millis_part: 0,
                    late_finalize_credits,
                    compressed_entities_root: None,
                })
                .map_err(|e| eyre::eyre!(e.to_string()))?
            };

        let attrs = OutbePayloadAttributes::new(
            REWARDS_ADDRESS,
            timestamp_millis,
            prev_randao,
            Some(B256::ZERO),
            header_extra_data,
            parent_consensus_metadata,
            self.proposer_evm_address,
        )
        .with_execution_read_budget(execution_read_budget);

        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before FCU payload build"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        // FCU-based payload building: canonicalize parent and start building
        // in one atomic operation via the executor actor.
        let payload_id = self
            .executor_mailbox
            .canonicalize_and_build(parent_height, parent_digest, attrs)
            .await
            .map_err(|e| eyre::eyre!("canonicalize_and_build failed: {e}"))?;
        payload_trace.record(payload_id);

        debug!(%payload_id, "payload building started via FCU");

        // Give the payload builder a bounded chance to execute transactions before
        // resolving. Elapsed is measured against the runtime clock (same source as
        // the sleep below), so it is correct on the deterministic runtime too.
        let elapsed = clock
            .current()
            .duration_since(propose_start)
            .unwrap_or_default();
        let remaining_resolve = self.payload_resolve_time.saturating_sub(elapsed);

        clock.sleep(remaining_resolve).await;

        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal after payload build started"
            );
            return Ok(BuildBlockOutcome::EpochStale);
        }

        let payload = self
            .payload_builder
            .resolve_kind(
                payload_id,
                reth_payload_builder::PayloadKind::WaitForPending,
            )
            .await
            .ok_or_else(|| eyre::eyre!("payload resolution returned None"))?
            .map_err(|e| eyre::eyre!("payload resolution failed: {e}"))?;

        let sealed_block = payload.block().clone();

        let consensus_block = ConsensusBlock::from_sealed(sealed_block);
        let digest = consensus_block.digest();
        let block_number = consensus_block.number();
        debug!(%digest, number = block_number, "block built");

        crate::metrics::record_block_proposed(block_number);

        self.block_cache
            .insert_bounded(digest, consensus_block.clone());

        Ok(BuildBlockOutcome::Built(digest, consensus_block))
    }
}
