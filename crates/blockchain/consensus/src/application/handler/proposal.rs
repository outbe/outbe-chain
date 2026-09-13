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

use crate::finalization::parent_cert_store::CertifiedParentProofKey;

use crate::finalization::state::FinalizationViewAccess;

use crate::ocomp_retention::OcompRetentionHook;

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
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

use outbe_primitives::reshare_artifact::OutbeBlockArtifacts;

use outbe_primitives::OutbeExecutionData;
use outbe_primitives::OutbePayloadAttributes;

use reth_node_builder::BuiltPayload as _;

use std::sync::Arc;

use tracing::debug;

use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProposeOutcome {
    Proposed(Digest),
    ParentProofUnavailable,
    EpochStale,
    BoundaryUnavailable,
    ProjectionUnavailable,
    RetentionUnavailable,
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
    #[error("node-local OCOMP retention is unavailable: {0}")]
    Retention(#[from] crate::ocomp_retention::OcompRetentionHookError),
}

/// Import a locally built candidate into Reth before its retention source reads
/// receipts and exact post-state by block hash.
pub(super) async fn prepare_built_candidate(
    engine: &EngineHandle,
    retention: &dyn OcompRetentionHook,
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
    retention.prepare_candidate(block)?;
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
        context: super::ingress::SimplexContext,
        propose_start: std::time::SystemTime,
        execution_read_budget: ExecutionReadBudget,
        payload_trace: ProposalPayloadTrace,
    ) -> eyre::Result<ProposeOutcome> {
        let (parent_view, parent) = context.parent;
        let parent_digest = Digest(parent.0);
        let round = context.round;
        debug!(%round, %parent_view, parent = %parent_digest.0, "propose requested");

        // epoch continuity: special-case the first proposal of a
        // new Simplex epoch (`epoch > 0`, `parent_view = 0`) before the chain
        // genesis path. `Ok(None)` means "not an epoch boundary"; caller falls
        // through to the chain genesis / cache / marshal-by-digest branches.
        let maybe_epoch_anchor = match epoch_boundary::resolve_epoch_boundary_parent(
            &self.finalization_view,
            &self.marshal_mailbox,
            clock,
            round,
            parent_view,
            parent_digest,
        )
        .await
        {
            Ok(opt) => opt,
            Err(error) => {
                warn!(
                    %round,
                    parent = %parent_digest.0,
                    %error,
                    "propose: epoch boundary parent resolution failed; forfeiting slot"
                );
                return Ok(ProposeOutcome::ParentProofUnavailable);
            }
        };
        debug_assert!(
            !(round.epoch().get() > 0
                && parent_view == View::new(0)
                && maybe_epoch_anchor.is_none()),
            "resolve_epoch_boundary_parent invariant: epoch>0 && parent_view=0 must \
             resolve to Some(EpochBoundaryParent) or return an explicit error"
        );

        let (parent_height, parent_block, parent_proof_key) =
            if let Some(anchor) = maybe_epoch_anchor {
                (anchor.height, Some(anchor.block), Some(anchor.proof_key))
            } else if parent_digest.0 == self.genesis_hash {
                (Height::zero(), None, None)
            } else {
                let cached_parent = self.block_cache.get_and_remove(&parent_digest);
                let parent_block = if let Some(block) = cached_parent {
                    block
                } else {
                    // Parent from another proposer - resolve via marshal.
                    let marshal = self.marshal_mailbox.clone();
                    let block_future = marshal.subscribe_by_digest(
                        parent_digest,
                        commonware_consensus::marshal::core::DigestFallback::FetchByRound {
                            round: parent_round(round, parent_view),
                        },
                    );
                    match clock
                        .timeout(PROPOSE_RESOLUTION_TIMEOUT, block_future)
                        .await
                    {
                        Ok(Ok(block)) => (*block).clone(),
                        Ok(Err(_)) => {
                            return Err(eyre::eyre!(
                                "failed to resolve parent block {} for proposal",
                                parent_digest.0
                            ));
                        }
                        Err(_) => {
                            return Err(eyre::eyre!(
                                "timed out resolving parent block {} for proposal",
                                parent_digest.0
                            ));
                        }
                    }
                };

                let parent_height = Height::new(parent_block.number());
                (
                    parent_height,
                    Some(parent_block),
                    Some(CertifiedParentProofKey::new(
                        parent_round(round, parent_view).epoch().get(),
                        parent_view.get(),
                        parent_digest.0,
                    )),
                )
            };

        let next_block_number = parent_height.get().saturating_add(1);
        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before Engine API work"
            );
            return Ok(ProposeOutcome::EpochStale);
        }

        let required_parent = ProjectionCheckpoint {
            block_number: parent_height.get(),
            block_hash: parent_digest.0,
        };
        if wait_for_projected_parent(
            self.projection_readiness.clone(),
            required_parent,
            std::future::pending(),
        )
        .await?
            == ParentProjectionGate::Withhold
        {
            return Ok(ProposeOutcome::ProjectionUnavailable);
        }

        if let Some(parent_block) = parent_block.as_ref() {
            // Step 2: Send parent to execution layer via new_payload.
            let execution_data =
                OutbeExecutionData::new(std::sync::Arc::new(parent_block.clone().into_inner()))
                    .with_execution_read_budget(execution_read_budget.clone());

            if crate::test_faults::should_drop_new_payload_for_test(parent_height) {
                warn!(
                    height = %parent_height,
                    parent = %parent_digest.0,
                    "test-marshal-drop: skipping propose parent new_payload"
                );
            } else {
                match self.engine.new_payload(execution_data).await {
                    Ok(status) if status.is_valid() || status.is_syncing() => {
                        debug!(parent = %parent_digest.0, ?status, "parent verified by execution layer");
                    }
                    Ok(status) => {
                        return Err(eyre::eyre!(
                            "parent {} rejected by execution layer: {status:?}",
                            parent_digest.0
                        ));
                    }
                    Err(e) => {
                        return Err(eyre::eyre!(
                            "new_payload failed for parent {}: {e}",
                            parent_digest.0
                        ));
                    }
                }
            }

            self.finalization_view
                .advance_timestamp_floor(parent_block.timestamp_millis());
        }

        self.vrf_safety
            .ensure_block_allowed(next_block_number)
            .map_err(|error| eyre::eyre!("refusing proposal above VRF expiry: {error}"))?;

        // Steps 3+4: Canonicalize parent as head and build next block.
        // Uses FCU-based flow: canonicalize_and_build sends
        // FCU with payload attributes so the engine starts building a payload
        // on the correct canonical state with access to the txpool.
        let candidate_execution_budget = execution_read_budget.clone();
        match self
            .build_block(
                clock,
                round,
                parent_height,
                parent_digest,
                parent_block.clone(),
                parent_proof_key,
                propose_start,
                execution_read_budget,
                payload_trace,
            )
            .await
        {
            Ok(BuildBlockOutcome::Built(digest, block)) => {
                if let Err(error) = self
                    .prepare_built_candidate(&block, candidate_execution_budget)
                    .await
                {
                    warn!(
                        %round,
                        digest = %digest.0,
                        %error,
                        "withholding proposal because its execution-valid OCOMP source is not durable"
                    );
                    return Ok(ProposeOutcome::RetentionUnavailable);
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
            Ok(BuildBlockOutcome::ParentProofUnavailable) => {
                Ok(ProposeOutcome::ParentProofUnavailable)
            }
            Ok(BuildBlockOutcome::EpochStale) => Ok(ProposeOutcome::EpochStale),
            Ok(BuildBlockOutcome::BoundaryUnavailable) => Ok(ProposeOutcome::BoundaryUnavailable),
            Err(e) => Err(eyre::eyre!("failed to build block for proposal: {e}")),
        }
    }

    async fn prepare_built_candidate(
        &self,
        block: &ConsensusBlock,
        execution_read_budget: ExecutionReadBudget,
    ) -> Result<(), BuiltCandidatePreparationError> {
        prepare_built_candidate(
            &self.engine,
            self.ocomp_retention.as_ref(),
            block,
            execution_read_budget,
        )
        .await
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
        let pending_boundary = self
            .dkg_manager
            .pending_boundary_artifact(round.epoch())
            .await;
        let ancestry = super::ancestry::marshal_ancestry_reader(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            Some(round),
            PROPOSE_RESOLUTION_TIMEOUT,
            clock.child("ancestry"),
        );
        let consensus_header_artifact = match self
            .dkg_manager
            .resolve_boundary(parent_block.as_ref(), pending_boundary.as_ref(), &ancestry)
            .await
        {
            Ok(BoundaryRequirement::AlreadyCommitted) => {
                crate::metrics::record_dkg_boundary_requirement(
                    crate::metrics::DkgBoundaryDecision::AlreadyCommitted,
                );
                None
            }
            Ok(BoundaryRequirement::MustEmit) => {
                let Some(boundary) = pending_boundary else {
                    return Err(eyre::eyre!(
                        "boundary requirement requested emission without pending artifact"
                    ));
                };
                crate::metrics::record_dkg_boundary_requirement(
                    crate::metrics::DkgBoundaryDecision::MustEmit,
                );
                Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))
            }
            Ok(BoundaryRequirement::NoPending) if proposed_height == 1 => {
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
            Ok(BoundaryRequirement::NoPending) => {
                crate::metrics::record_dkg_boundary_requirement(
                    crate::metrics::DkgBoundaryDecision::NoPending,
                );
                // If the DKG for the NEXT epoch has completed (its boundary is
                // pending) but this is not yet its activation block, PRE-ANNOUNCE
                // that committee in this E-1 block so a follower authenticates it via
                // the already-trusted E-1 committee - before the self-finalized
                // activation boundary at E*L+1 (Path A committee-chaining). Otherwise
                // a DKG is still in flight, so emit a dealer log.
                if let Some(boundary) = self
                    .dkg_manager
                    .pending_next_epoch_artifact(round.epoch())
                    .await
                {
                    Some(ConsensusHeaderArtifact::CommitteePreAnnounce {
                        epoch: boundary.epoch,
                        outcome: boundary.outcome,
                    })
                } else {
                    self.dkg_manager
                        .get_dealer_log(round.epoch())
                        .await
                        .map(ConsensusHeaderArtifact::DealerLog)
                }
            }
            Err(error) => {
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
