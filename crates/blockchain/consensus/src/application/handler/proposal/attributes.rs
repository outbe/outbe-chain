use super::*;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;
use std::ops::ControlFlow;

impl ApplicationShared {
    pub(super) async fn proposal_attributes(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: &BlockBuildRequest,
    ) -> eyre::Result<ControlFlow<BuildBlockOutcome, OutbePayloadAttributes>> {
        let round = request.round;
        let parent_height = request.parent.height;
        let parent_digest = request.parent.digest;
        let parent_block = &request.parent.block;
        let parent_proof_key = request.parent.proof_key;
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
        let consensus_header_artifact = match self.proposal_header_artifact(clock, request).await? {
            ControlFlow::Continue(artifact) => artifact,
            ControlFlow::Break(outcome) => return Ok(ControlFlow::Break(outcome)),
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
            ParentProofLookup::Unavailable => {
                return Ok(ControlFlow::Break(
                    BuildBlockOutcome::ParentProofUnavailable,
                ))
            }
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

        let header_extra_data =
            self.encode_proposal_header(proposed_height, consensus_header_artifact)?;
        let attrs = OutbePayloadAttributes::new(outbe_primitives::OutbePayloadAttributesInput {
            suggested_fee_recipient: REWARDS_ADDRESS,
            timestamp_millis,
            prev_randao,
            parent_beacon_block_root: Some(B256::ZERO),
            extra_data: header_extra_data,
            parent_consensus_metadata,
            proposer_evm_address: self.proposer_evm_address,
        })
        .with_execution_read_budget(request.execution_read_budget.clone());

        Ok(ControlFlow::Continue(attrs))
    }
    async fn proposal_header_artifact(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: &BlockBuildRequest,
    ) -> eyre::Result<ControlFlow<BuildBlockOutcome, Option<ConsensusHeaderArtifact>>> {
        let round = request.round;
        let parent_block = &request.parent.block;
        let proposed_height = request.parent.height.get().saturating_add(1);
        let ancestry = super::super::ancestry::marshal_ancestry_reader(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            crate::application::ancestry::AncestryLookupPolicy {
                round: Some(round),
                timeout: PROPOSE_RESOLUTION_TIMEOUT,
            },
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
            Err(forfeit) => {
                return Ok(ControlFlow::Break(Self::proposal_header_forfeit(
                    round,
                    proposed_height,
                    forfeit,
                )))
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
        let consensus_header_artifact = super::super::byzantine_hook::override_artifact(
            plan.requirement,
            consensus_header_artifact,
        );

        Ok(ControlFlow::Continue(consensus_header_artifact))
    }
    fn proposal_header_forfeit(
        round: Round,
        proposed_height: u64,
        forfeit: ProposalForfeit,
    ) -> BuildBlockOutcome {
        match forfeit {
            ProposalForfeit::GenesisBoundaryNotReady => {
                debug!(
                    %round,
                    proposed_height,
                    "block 1 proposal forfeited: DKG boundary artifact for epoch 0 not ready"
                );
                crate::metrics::record_genesis_dkg_boundary_not_ready_forfeit();
                crate::metrics::record_dkg_boundary_unavailable(
                    crate::metrics::DkgBoundaryUnavailableReason::GenesisBoundaryNotReady,
                );
            }
            ProposalForfeit::Boundary(error) => {
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
            }
        }
        BuildBlockOutcome::BoundaryUnavailable
    }
    fn encode_proposal_header(
        &self,
        proposed_height: u64,
        consensus_header_artifact: Option<ConsensusHeaderArtifact>,
    ) -> eyre::Result<Bytes> {
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

        Ok(header_extra_data)
    }
}
