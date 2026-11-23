use super::*;
use crate::dkg_manager::AncestryReader;
use crate::finalization::late_sig_store::{FinalizeVoteTarget, LateCreditBranchTarget};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;
use std::ops::ControlFlow;

// Older late-credit targets are optional; a missing ancestor must not consume
// the proposal's normal two-second resolution budget for each credit window slot.
const LATE_CREDIT_ANCESTOR_LOOKUP_TIMEOUT: Duration = Duration::from_millis(100);

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
        let timestamp_millis = resolve_proposal_timestamp_millis(parent_block.as_ref(), now_millis);

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
        // `genesis_dkg_boundary_not_ready` reason. Never propose block 1
        // without a real boundary artifact.
        let proposed_height = parent_height.get().saturating_add(1);
        let consensus_header_artifact = match self.proposal_header_artifact(clock, request).await? {
            ControlFlow::Continue(artifact) => artifact,
            ControlFlow::Break(outcome) => return Ok(ControlFlow::Break(outcome)),
        };
        // Direct-parent proof selection
        // (finalization first -> certified-notarization -> marshal-archive
        // recovery -> forfeit). The request budget does not gate this lookup.
        // The lookup can wait. When the store holds no record yet, the selector
        // waits a bounded time for one: the reporter only queues the certificate.
        // When it holds only a certified-notarization record, the selector waits
        // a bounded time for a finalization record.
        // The wait starts at `PHASE1_FINALIZATION_WAIT_DEFAULT`, and the default
        // leader timeout caps it. On a selection-store miss, the None branch
        // recovers the parent's finalization from marshal's durable archive
        // (`recover_parent_proof_from_marshal`). Only if that also misses does
        // the slot forfeit deterministically with the parent-proof-unavailable
        // metric.
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
        // metadata. `ParentProofSelector::select_direct_parent_proof_by_key_with_wait`
        // is the upstream selector that decides which record (if any) to feed
        // into Phase 1.
        // The selector guarantees the chosen record's height resolves to
        // `parent_height`. A Finalization record is validated to match. A
        // CertifiedNotarization record carries no height of its own, and this
        // code resolves it to the parent.
        let parent_consensus_metadata = parent_proof_record
            .as_ref()
            .map(|record| record.to_v2_metadata(parent_height.get()));
        // Bind randomness to the same exact-parent certificate as Phase 1.
        // A process-local last-finalized seed can lag, advance, or reset on
        // restart. The genesis child has no certificate and uses ZERO.
        let prev_randao = parent_consensus_metadata
            .as_ref()
            .map(crate::proof::verifier::decode_parent_prev_randao)
            .transpose()?
            .unwrap_or(B256::ZERO);
        if parent_consensus_metadata.is_some() {
            crate::metrics::record_parent_cert_included();
        }

        let late_credit_targets = self
            .late_credit_targets_for_branch(clock, request, parent_consensus_metadata.as_ref())
            .await?;
        let header_extra_data = self.encode_proposal_header(
            proposed_height,
            consensus_header_artifact,
            &late_credit_targets,
        )?;
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
        late_credit_targets: &[LateCreditBranchTarget],
    ) -> eyre::Result<Bytes> {
        // pack the in-window late-finalize credits this node has
        // locally observed for blocks `proposed_height - K ..= proposed_height - 1`.
        // Best-effort and process-local. Every validator re-verifies each batch
        // (pre-exec FATAL) and re-derives the same artifact via header<->calldata
        // parity. Thus the contents never affect determinism. An empty store just
        // credits nobody. A poisoned lock degrades to no credits.
        let late_finalize_credits = match self.late_sig_store.lock() {
            Ok(store) => {
                let artifact =
                    store.build_artifact_for_branch(proposed_height, late_credit_targets);
                if artifact.batches.is_empty() {
                    None
                } else {
                    Some(artifact)
                }
            }
            Err(_) => None,
        };

        let header_extra_data =
            outbe_primitives::reshare_artifact::encode_proposal_block_artifacts(
                OutbeBlockArtifacts {
                    radicle_publication_enabled: self.repository_publication_enabled,
                    radicle_publication: None,
                    execution_summary: None,
                    consensus_header_artifact,
                    // The payload builder recomputes the sub-second timestamp
                    // part from `OutbeBlockExecutionCtx` and re-encodes it into
                    // `extra_data` before sealing. We intentionally leave it
                    // at 0 here.
                    timestamp_millis_part: 0,
                    late_finalize_credits,
                    enclave_witness: None,
                },
                &self.repository_publication,
            )?;

        Ok(header_extra_data)
    }

    async fn late_credit_targets_for_branch(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: &BlockBuildRequest,
        parent_metadata: Option<&CertifiedParentAccountingMetadata>,
    ) -> eyre::Result<Vec<LateCreditBranchTarget>> {
        let window_k = self
            .late_sig_store
            .lock()
            .map(|store| store.window_k())
            .unwrap_or(0);
        let ancestry = super::super::ancestry::marshal_ancestry_reader(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            crate::application::ancestry::AncestryLookupPolicy {
                // The ancestor's round is not the current proposal round.
                // Use a local/cache wait only; optional credits can be omitted.
                round: None,
                timeout: LATE_CREDIT_ANCESTOR_LOOKUP_TIMEOUT,
            },
            clock.child("late_credit_ancestry"),
        );
        collect_branch_targets(
            &ancestry,
            window_k,
            request.parent.height.get(),
            request.parent.digest.0,
            request.parent.block.clone(),
            parent_metadata,
        )
        .await
    }
}

async fn collect_branch_targets(
    ancestry: &impl AncestryReader,
    window_k: u64,
    parent_height: u64,
    parent_hash: B256,
    parent_block: Option<ConsensusBlock>,
    parent_metadata: Option<&CertifiedParentAccountingMetadata>,
) -> eyre::Result<Vec<LateCreditBranchTarget>> {
    let mut targets = Vec::new();
    if let Some(metadata) = parent_metadata {
        if metadata.finalized_block_number != parent_height
            || metadata.finalized_block_hash != parent_hash
        {
            return Err(eyre::eyre!(
                "selected parent proof is not bound to proposal parent"
            ));
        }
        targets.push(branch_target_from_metadata(metadata));
    }
    let Some(mut child) = parent_block else {
        return Ok(targets);
    };
    if child.number() != parent_height || child.block_hash() != parent_hash {
        return Err(eyre::eyre!("resolved proposal parent identity mismatch"));
    }
    // The selected proof in N describes N-1. Each existing branch child
    // describes its own exact parent, so the loop covers N-2 through N-K.
    for index in 1..window_k {
        if child.number() < 2 {
            break;
        }
        let metadata =
            match super::super::parent_proof::certified_parent_metadata_from_phase1_system_tx(
                &child,
            ) {
                Ok(Some(metadata)) => metadata,
                Ok(None) => {
                    crate::metrics::propose::record_late_credit_ancestor_omitted();
                    warn!(
                        child_height = child.number(),
                        "optional late-credit child has no Phase1 parent proof"
                    );
                    break;
                }
                Err(error) => {
                    crate::metrics::propose::record_late_credit_ancestor_omitted();
                    warn!(child_height = child.number(), %error, "optional late-credit ancestor Phase1 unavailable");
                    break;
                }
            };
        if metadata.finalized_block_number != child.number() - 1
            || metadata.finalized_block_hash != child.parent_hash()
        {
            crate::metrics::propose::record_late_credit_ancestor_omitted();
            warn!(
                child_height = child.number(),
                metadata_parent_height = metadata.finalized_block_number,
                metadata_parent_hash = %metadata.finalized_block_hash,
                actual_parent_hash = %child.parent_hash(),
                "optional late-credit Phase1 parent binding mismatch"
            );
            break;
        }
        targets.push(branch_target_from_metadata(&metadata));
        if index + 1 == window_k {
            break;
        }
        let expected_hash = child.parent_hash();
        let expected_height = child.number() - 1;
        let next = match ancestry.get_block_by_height(expected_height).await {
            Some(block) if block.block_hash() == expected_hash => Some(block),
            _ => ancestry.get_block_by_hash(expected_hash).await,
        };
        let Some(next) = next else {
            crate::metrics::propose::record_late_credit_ancestor_omitted();
            break;
        };
        if next.number() != expected_height || next.block_hash() != expected_hash {
            crate::metrics::propose::record_late_credit_ancestor_omitted();
            warn!(
                expected_height,
                expected_hash = %expected_hash,
                actual_height = next.number(),
                actual_hash = %next.block_hash(),
                "optional late-credit ancestor identity mismatch"
            );
            break;
        }
        child = next;
    }
    Ok(targets)
}

fn branch_target_from_metadata(
    metadata: &CertifiedParentAccountingMetadata,
) -> LateCreditBranchTarget {
    LateCreditBranchTarget {
        fb_number: metadata.finalized_block_number,
        binding: FinalizeVoteTarget {
            epoch: metadata.finalized_epoch,
            view: metadata.finalized_view,
            parent_view: metadata.parent_view,
            fb_hash: metadata.finalized_block_hash,
        },
        committee_set_hash: metadata.committee_set_hash,
        committee_size: metadata.ordered_committee.len(),
    }
}

fn resolve_proposal_timestamp_millis(
    parent_block: Option<&ConsensusBlock>,
    now_millis: u64,
) -> u64 {
    if super::super::timing::parent_ahead_of_local_clock(parent_block, now_millis) {
        crate::metrics::propose::record_parent_ahead_of_local_clock();
    }
    // Clamp the proposed timestamp into the deterministic drift band
    // `[parent + 1, parent + MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS]`. The lower
    // bound enforces strict +1 ms monotonicity over the parent. The upper
    // bound (C-01) mirrors the validator check in `outbe-execution`'s
    // `validate_against_parent_timestamp_millis`. Thus an honest proposer
    // never emits a block that validators would reject as over-drifted. After
    // a long stall `now_millis` may exceed the cap. The chain then self-heals:
    // it ratchets time forward by at most one band per block until it catches
    // up to real time.
    //
    // Exception: when no parent block was resolved (`parent_block == None`),
    // `proposal_timestamp_millis` passes 0 and applies only `max(now_millis, 1)`
    // without clamping to `0 + band`.
    proposal_timestamp_millis(
        parent_block,
        now_millis,
        outbe_primitives::consensus::MAX_BLOCK_TIMESTAMP_DRIFT_MILLIS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{block_with_system_inputs, SystemBlockFixture};
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use outbe_primitives::signer::OutbeEvmSigner;
    use outbe_primitives::system_tx::{SystemTx, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX};

    struct NoAncestors;

    impl AncestryReader for NoAncestors {
        fn get_block_by_height<'a>(&'a self, _: u64) -> crate::dkg_manager::BlockLookupFuture<'a> {
            Box::pin(async { panic!("optional ancestor lookup must not be reached") })
        }

        fn get_block_by_hash<'a>(&'a self, _: B256) -> crate::dkg_manager::BlockLookupFuture<'a> {
            Box::pin(async { panic!("optional ancestor lookup must not be reached") })
        }

        fn is_ready(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn malformed_optional_ancestor_phase1_keeps_exact_parent_target() {
        let parent = crate::test_fixtures::block_with_number(2);
        let hash = parent.block_hash();
        let metadata = CertifiedParentAccountingMetadata {
            finalized_block_number: 2,
            finalized_block_hash: hash,
            finalized_epoch: 1,
            finalized_view: 7,
            parent_view: 6,
            committee_set_hash: B256::repeat_byte(0x42),
            ..Default::default()
        };

        // The synthetic parent has no Phase1. Its own exact-parent target is
        // still available, and the optional older target is omitted.
        let targets =
            collect_branch_targets(&NoAncestors, 3, 2, hash, Some(parent), Some(&metadata))
                .await
                .expect("optional credit failure must not forfeit the proposal");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], branch_target_from_metadata(&metadata));
    }

    #[tokio::test]
    async fn branch_targets_cover_exact_parent_and_older_phase1_escrow() {
        let block_one = crate::test_fixtures::block_with_number(1);
        let block_one_hash = block_one.block_hash();
        let metadata_one = CertifiedParentAccountingMetadata {
            finalized_block_number: 1,
            finalized_block_hash: block_one_hash,
            finalized_epoch: 2,
            finalized_view: 7,
            parent_view: 6,
            committee_set_hash: B256::repeat_byte(0x41),
            ..Default::default()
        };
        let signer = OutbeEvmSigner::from_secret_bytes([7; 32]).unwrap();
        let block_two = block_with_system_inputs(
            &signer,
            SystemBlockFixture {
                block_number: 2,
                parent_hash: block_one_hash,
                extra_data: Bytes::new(),
                inputs: vec![SystemTx::Consensus {
                    step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
                    payload: metadata_one.encode().unwrap(),
                }],
                chain_id: outbe_primitives::chain::CHAIN_ID,
            },
        );
        let metadata_two = CertifiedParentAccountingMetadata {
            finalized_block_number: 2,
            finalized_block_hash: block_two.block_hash(),
            finalized_epoch: 2,
            finalized_view: 8,
            parent_view: 7,
            committee_set_hash: B256::repeat_byte(0x42),
            ..Default::default()
        };

        let at_two = collect_branch_targets(
            &NoAncestors,
            2,
            1,
            block_one_hash,
            Some(block_one),
            Some(&metadata_one),
        )
        .await
        .unwrap();
        assert_eq!(at_two, vec![branch_target_from_metadata(&metadata_one)]);

        let at_three = collect_branch_targets(
            &NoAncestors,
            2,
            2,
            block_two.block_hash(),
            Some(block_two),
            Some(&metadata_two),
        )
        .await
        .unwrap();
        assert_eq!(
            at_three,
            vec![
                branch_target_from_metadata(&metadata_two),
                branch_target_from_metadata(&metadata_one),
            ]
        );
    }

    #[test]
    fn timestamp_resolution_records_only_a_parent_strictly_ahead_of_local_time() {
        let mut block = crate::test_fixtures::block_with_number(1)
            .into_inner()
            .into_block();
        block.header.inner.timestamp = 3;
        let parent =
            ConsensusBlock::from_sealed(reth_ethereum::primitives::SealedBlock::seal_slow(block));
        assert_eq!(parent.timestamp_millis(), 3000);
        for (now, expected_timestamp, ahead) in [
            (2999, 3001, true),
            (3000, 3001, false),
            (3001, 3001, false),
            (3999, 3999, false),
            (4000, 4000, false),
            (4001, 4001, false),
        ] {
            let recorder = DebuggingRecorder::new();
            let timestamp = metrics::with_local_recorder(&recorder, || {
                resolve_proposal_timestamp_millis(Some(&parent), now)
            });
            assert_eq!(timestamp, expected_timestamp);
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            assert_eq!(snapshot.len(), usize::from(ahead));
            if ahead {
                assert_eq!(
                    snapshot[0].0.key().name(),
                    "outbe_consensus_parent_ahead_of_local_clock_total"
                );
                assert_eq!(snapshot[0].0.key().labels().count(), 0);
                assert_eq!(snapshot[0].3, DebugValue::Counter(1));
            }
        }
    }
}
