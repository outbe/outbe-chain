//! Block application, certificate persistence, and per-height side effects for `FinalizationActor`.

use commonware_consensus::types::{Epoch, Height, Round};
use outbe_primitives::{
    consensus::{ConsensusData, ConsensusStatus},
    reshare_artifact::ConsensusHeaderArtifact,
};
use tracing::{debug, warn};

use super::{
    notify_finalized_waiters, persist_with_budget, ApplyOutcome, FinalizationActor,
    PersistFinalizationOutcome, ProofPersistFailure, PARENT_CERT_KEEP_DEPTH,
};
use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::committee_prelude::build_committee_prelude;
use crate::finalization::evidence::VerifiedChildParentWitness;
use crate::finalization::ingress::Finalized;
use crate::finalization::late_sig_store::{FinalizeVoteTarget, LateFinalizeCommittee};
use crate::finalization::parent_cert_store::{
    CertifiedParentProofKey, CertifiedParentProofRecord, CertifiedParentProofStore, ProofKind,
    CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
};
use crate::finalization::state::FinalizationView;
use crate::finalization::util::{
    classify_finalization, extract_header_artifact_from_block, ReplayClassification,
};

enum FinalizationAction {
    DropApplied,
    PersistDuplicate,
    ApplyNew,
}

struct BlockEffectsInput<'a> {
    block: &'a ConsensusBlock,
    digest: Digest,
    block_number: u64,
    round: Round,
    consensus_data: Option<&'a ConsensusData>,
    vrf_seed: Option<alloy_primitives::B256>,
}

struct CommitteePreludeFields {
    committee_set_hash: alloy_primitives::B256,
    vrf_material_version: u64,
    vrf_group_public_key_hash: alloy_primitives::B256,
}

impl FinalizationActor {
    /// Process a genuine `Finalization` for which we have the full block.
    ///
    /// Splits per-block effects from per-proof effects.
    /// - `ReplayClassification::New`: attempts the local proof write, then
    ///   publishes the authenticated block even if the local handoff fails.
    /// - `ReplayClassification::DuplicateReplay` (same height & hash, e.g.
    ///   same-hash `F(v2)` after `F(v1)` or repair of a missing proof): persists
    ///   the proof and advances `view.last_finalized_round` ONLY when the proof
    ///   is actually available in `parent_cert_store`. Never repeats per-block
    ///   effects (`DealerLog`, `BoundaryOutcome`, `view_advanced`, status, eviction).
    pub(super) async fn process_finalization(
        &self,
        finalized: Finalized,
        block: ConsensusBlock,
    ) -> eyre::Result<ApplyOutcome> {
        let digest = finalized.digest;
        let block_number = block.number();
        let mut view = self.deps.view.write();

        let is_duplicate =
            match self.classify_finalization_action(&mut view, &finalized, digest, block_number)? {
                FinalizationAction::DropApplied => return Ok(ApplyOutcome::Applied),
                FinalizationAction::PersistDuplicate => true,
                FinalizationAction::ApplyNew => false,
            };

        let consensus_data = finalized.consensus_data.clone();
        let outcome =
            self.persist_finalization_record(&finalized, &consensus_data, digest, block_number);
        if let Some(early) = check_persist_early_return(&outcome, is_duplicate) {
            return Ok(early);
        }
        self.finish_finalization_persistence(
            &view,
            &finalized,
            &consensus_data,
            &outcome,
            block_number,
        );
        if is_duplicate {
            advance_round_and_optional_seed(&mut view, finalized.round, finalized.vrf_seed);
            return Ok(ApplyOutcome::Applied);
        }

        advance_finalized_view(&mut view, finalized.round, digest, &block);
        if let Some(seed) = finalized.vrf_seed {
            view.prev_randao = seed;
        } else {
            self.deps.vrf_safety.mark_degraded();
        }
        drop(view);

        self.apply_block_effects(BlockEffectsInput {
            block: &block,
            digest,
            block_number,
            round: finalized.round,
            consensus_data: Some(&consensus_data),
            vrf_seed: finalized.vrf_seed,
        })?;
        Ok(ApplyOutcome::Applied)
    }

    fn classify_finalization_action(
        &self,
        view: &mut FinalizationView,
        finalized: &Finalized,
        digest: Digest,
        block_number: u64,
    ) -> eyre::Result<FinalizationAction> {
        match classify_finalization(
            block_number,
            digest.0,
            view.last_finalized_number,
            view.forkchoice.finalized_block_hash,
        ) {
            ReplayClassification::HistoricalReplay => {
                debug!(
                    block_number,
                    last_finalized = view.last_finalized_number,
                    %digest,
                    "dropping replayed finalization (historical)"
                );
                Ok(FinalizationAction::DropApplied)
            }
            ReplayClassification::DuplicateReplay => {
                Ok(self.classify_duplicate_finalization(view, finalized, digest, block_number))
            }
            ReplayClassification::FatalInconsistency => {
                Err(fatal_same_height_inconsistency(view, block_number, digest))
            }
            ReplayClassification::New => Ok(FinalizationAction::ApplyNew),
        }
    }

    fn classify_duplicate_finalization(
        &self,
        view: &mut FinalizationView,
        finalized: &Finalized,
        digest: Digest,
        block_number: u64,
    ) -> FinalizationAction {
        let proof_key = CertifiedParentProofKey::new(
            finalized.round.epoch().get(),
            finalized.round.view().get(),
            digest.0,
        );
        if self
            .deps
            .parent_cert_store
            .get_finalization(proof_key)
            .is_some()
        {
            advance_last_finalized_round(view, finalized.round);
            debug!(
                block_number,
                %digest,
                "dropping replayed finalization (duplicate)"
            );
            return FinalizationAction::DropApplied;
        }
        debug!(
            block_number,
            %digest,
            "persisting same-height finalization proof without repeating block effects"
        );
        FinalizationAction::PersistDuplicate
    }

    fn finish_finalization_persistence(
        &self,
        view: &FinalizationView,
        finalized: &Finalized,
        consensus_data: &ConsensusData,
        outcome: &PersistFinalizationOutcome,
        block_number: u64,
    ) {
        if let Some(committee) = outcome.rekey_committee {
            self.rekey_late_finalize_votes(
                FinalizeVoteTarget {
                    epoch: finalized.round.epoch().get(),
                    view: finalized.round.view().get(),
                    parent_view: consensus_data.finalized_certificate.parent_view,
                    fb_hash: finalized.digest.0,
                },
                block_number,
                committee,
            );
        }
        let prune_succeeded = self.prune_parent_cert_store(
            block_number,
            view.last_finalized_round.unwrap_or(finalized.round),
        );
        if outcome.put_succeeded && prune_succeeded {
            crate::metrics::record_finalization_persist_recovered();
        }
    }

    /// Apply an implicit finalized height whose verified immediate child Phase 1
    /// witness is a `CertifiedNotarization`.
    pub(super) async fn apply_implicit_cn(
        &self,
        witness: VerifiedChildParentWitness,
        block: ConsensusBlock,
    ) -> eyre::Result<ApplyOutcome> {
        let digest = block.digest();
        let block_number = block.number();

        let mut view = self.deps.view.write();
        match classify_finalization(
            block_number,
            digest.0,
            view.last_finalized_number,
            view.forkchoice.finalized_block_hash,
        ) {
            ReplayClassification::HistoricalReplay | ReplayClassification::DuplicateReplay => {
                return Ok(ApplyOutcome::Applied);
            }
            ReplayClassification::FatalInconsistency => {
                return Err(fatal_same_height_inconsistency(&view, block_number, digest));
            }
            ReplayClassification::New => {}
        }

        let round = witness.round;
        let prev_randao = witness.prev_randao;
        let cn_record = build_cn_record(witness, digest);
        let put_succeeded = persist_with_budget("put_certified_notarization", || {
            self.deps
                .parent_cert_store
                .put_certified_notarization(cn_record.clone())
        })
        .is_some();
        let prune_succeeded =
            self.prune_parent_cert_store(block_number, view.last_finalized_round.unwrap_or(round));
        if put_succeeded && prune_succeeded {
            crate::metrics::record_finalization_persist_recovered();
        }

        advance_finalized_view(&mut view, round, digest, &block);
        view.prev_randao = prev_randao;
        drop(view);

        self.apply_block_effects(BlockEffectsInput {
            block: &block,
            digest,
            block_number,
            round,
            consensus_data: None,
            vrf_seed: Some(prev_randao),
        })?;
        Ok(ApplyOutcome::Applied)
    }

    /// Per-height block effects that must execute at most once per `(height, hash)`.
    fn apply_block_effects(&self, input: BlockEffectsInput<'_>) -> eyre::Result<()> {
        self.publish_consensus_status(
            input.round,
            input.consensus_data,
            input.vrf_seed,
            input.block_number,
        );
        self.note_finalized_dkg_artifact(input.block, input.digest, input.block_number);
        self.observe_finalized_boundary(input.block, input.block_number)?;
        notify_finalized_waiters(self.finalization_notify.as_deref());
        if let Some(tx) = &self.view_advanced {
            let _ = tx.send(input.block_number);
        }
        self.evict_finalized_block_cache();
        Ok(())
    }

    /// Build and persist the canonical V2 finalization parent-proof record.
    fn persist_finalization_record(
        &self,
        finalized: &Finalized,
        consensus_data: &ConsensusData,
        digest: Digest,
        block_number: u64,
    ) -> PersistFinalizationOutcome {
        let finalized_epoch = finalized.round.epoch().get();
        let ordered_committee = consensus_data
            .finalized_certificate
            .ordered_committee
            .clone();
        let committee_size = ordered_committee.len();
        let prelude = match self.resolve_committee_prelude(
            finalized.round.epoch(),
            &ordered_committee,
            block_number,
        ) {
            Ok(prelude) => prelude,
            Err(failure) => {
                return PersistFinalizationOutcome {
                    rekey_committee: None,
                    put_succeeded: false,
                    failure: Some(failure),
                };
            }
        };
        let snap = CertifiedParentProofRecord {
            format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
            kind: ProofKind::Finalization {
                finalized_block_number: block_number,
            },
            finalized_block_hash: digest.0,
            finalized_epoch,
            finalized_view: finalized.round.view().get(),
            parent_view: consensus_data.finalized_certificate.parent_view,
            ordered_committee,
            signer_bitmap: consensus_data.finalized_certificate.signer_bitmap.clone(),
            encoded_proof: consensus_data
                .finalized_certificate
                .encoded_certificate
                .clone(),
            committee_set_hash: prelude.committee_set_hash,
            vrf_material_version: prelude.vrf_material_version,
            vrf_group_public_key_hash: prelude.vrf_group_public_key_hash,
        };
        let put_succeeded = persist_with_budget("put_finalization", || {
            self.deps.parent_cert_store.put_finalization(snap.clone())
        })
        .is_some();
        PersistFinalizationOutcome {
            rekey_committee: Some(LateFinalizeCommittee {
                set_hash: prelude.committee_set_hash,
                size: committee_size,
            }),
            put_succeeded,
            failure: (!put_succeeded).then_some(ProofPersistFailure::Store),
        }
    }

    fn resolve_committee_prelude(
        &self,
        epoch: Epoch,
        ordered_committee: &[alloy_primitives::Address],
        block_number: u64,
    ) -> std::result::Result<CommitteePreludeFields, ProofPersistFailure> {
        let finalized_epoch = epoch.get();
        let Some(scheme) = self.deps.certificate_scheme_provider.scoped(epoch) else {
            crate::metrics::record_finalization_snapshot_build_degraded();
            warn!(
                target: "outbe::finalization",
                epoch = finalized_epoch,
                block_number,
                "no verifier scheme registered for finalized epoch; skipping finalization \
                 parent-cert write and keeping target pending"
            );
            return Err(ProofPersistFailure::MissingVerifier);
        };
        match build_committee_prelude(&scheme, ordered_committee, finalized_epoch) {
            Ok(prelude) => Ok(CommitteePreludeFields {
                committee_set_hash: prelude.committee_set_hash,
                vrf_material_version: prelude.vrf_material_version,
                vrf_group_public_key_hash: prelude.vrf_group_public_key_hash,
            }),
            Err(error) => {
                crate::metrics::record_finalization_snapshot_build_degraded();
                warn!(
                    target: "outbe::finalization",
                    epoch = finalized_epoch,
                    block_number,
                    %error,
                    "committee prelude build failed on finalization; skipping finalization parent-cert write in degraded mode"
                );
                Err(ProofPersistFailure::Metadata)
            }
        }
    }

    fn rekey_late_finalize_votes(
        &self,
        target: FinalizeVoteTarget,
        block_number: u64,
        committee: LateFinalizeCommittee,
    ) {
        if let Ok(mut store) = self.deps.late_sig_store.lock() {
            store.resolve_finalized_target(target, block_number, committee);
        }
    }

    fn prune_parent_cert_store(&self, block_number: u64, processed_round: Round) -> bool {
        let height_floor = block_number.saturating_sub(PARENT_CERT_KEEP_DEPTH);
        let pruned_height = persist_with_budget("prune_below_height", || {
            self.deps.parent_cert_store.prune_below_height(height_floor)
        });
        let keep_views = u64::from(crate::config::ACTIVITY_TIMEOUT)
            .saturating_mul(crate::config::VIEW_RETENTION_MULTIPLIER);
        let round_floor = Round::new(
            processed_round.epoch(),
            processed_round
                .view()
                .saturating_sub(commonware_consensus::types::ViewDelta::new(keep_views)),
        );
        let pruned_cn = persist_with_budget("prune_certified_notarizations_below_round", || {
            self.deps
                .parent_cert_store
                .prune_certified_notarizations_below_round(round_floor)
        });
        let pruned = pruned_height.unwrap_or(0) + pruned_cn.unwrap_or(0);
        crate::metrics::record_parent_cert_store_size(self.deps.parent_cert_store.len());
        crate::metrics::record_parent_cert_record_pruned(pruned);
        let retained_depth = self
            .deps
            .parent_cert_store
            .oldest_stored_height()
            .map_or(0, |oldest| block_number.saturating_sub(oldest));
        crate::metrics::record_parent_cert_retained_depth(retained_depth);
        pruned_height.is_some() && pruned_cn.is_some()
    }

    fn publish_consensus_status(
        &self,
        round: Round,
        consensus_data: Option<&ConsensusData>,
        vrf_seed: Option<alloy_primitives::B256>,
        block_number: u64,
    ) {
        let Some(ref bridge) = self.deps.bridge else {
            return;
        };
        let vrf_safety = self.deps.vrf_safety.snapshot();
        let connected_peers = consensus_data
            .map(|data| {
                u32::try_from(
                    data.finalized_certificate
                        .signer_bitmap
                        .iter()
                        .filter(|signed| **signed != 0)
                        .count(),
                )
                .unwrap_or(u32::MAX)
            })
            .unwrap_or(0);
        bridge.set_consensus_status(ConsensusStatus {
            current_view: round.view().get(),
            connected_peers,
            last_finalized_block: block_number,
            last_vrf_seed: vrf_seed,
            randomness_status: vrf_safety.randomness_status,
            vrf_material_version: vrf_safety.vrf_material_version,
            last_dkg_activation_height: vrf_safety.last_dkg_activation_height,
            next_planned_activation_height: vrf_safety.next_planned_activation_height,
            vrf_expiry_height: vrf_safety.vrf_expiry_height,
        });
    }

    fn observe_finalized_boundary(
        &self,
        block: &ConsensusBlock,
        height: u64,
    ) -> std::result::Result<(), crate::errors::FinalizationError> {
        let Some(epocher) = &self.epocher else {
            return Ok(());
        };
        let Ok(Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary))) =
            extract_header_artifact_from_block(block)
        else {
            return Ok(());
        };
        if boundary.epoch == 0 {
            return Ok(());
        }
        epocher
            .observe_boundary(Epoch::new(boundary.epoch), Height::new(height))
            .map_err(
                |source| crate::errors::FinalizationError::BoundaryObservationFailed {
                    epoch: boundary.epoch,
                    height,
                    source,
                },
            )
    }

    fn note_finalized_dkg_artifact(
        &self,
        block: &ConsensusBlock,
        digest: Digest,
        block_number: u64,
    ) {
        match extract_header_artifact_from_block(block) {
            Ok(artifact) => {
                self.deps.dkg_manager.note_finalized_header_artifact_at(
                    block_number,
                    digest.0,
                    artifact.as_ref(),
                );
            }
            Err(error) => {
                warn!(
                    %digest,
                    %error,
                    "finalized block carries invalid DKG header artifact"
                );
            }
        }
    }

    fn evict_finalized_block_cache(&self) {
        let finalized_num = self.deps.view.read().last_finalized_number;
        self.deps.block_cache.evict_at_or_below(finalized_num);
    }
}

fn check_persist_early_return(
    outcome: &PersistFinalizationOutcome,
    is_duplicate: bool,
) -> Option<ApplyOutcome> {
    if outcome.failure == Some(ProofPersistFailure::MissingVerifier) {
        return Some(ApplyOutcome::PendingVerifier);
    }
    if is_duplicate && !outcome.put_succeeded {
        return Some(match outcome.failure {
            Some(ProofPersistFailure::Store) => ApplyOutcome::PendingPersist,
            Some(ProofPersistFailure::Metadata) => ApplyOutcome::Applied,
            Some(ProofPersistFailure::MissingVerifier) | None => unreachable!(),
        });
    }
    None
}

fn fatal_same_height_inconsistency(
    view: &FinalizationView,
    block_number: u64,
    digest: Digest,
) -> eyre::Report {
    tracing::error!(
        block_number,
        %digest,
        finalized_hash = %view.forkchoice.finalized_block_hash,
        "fatal same-height finalization inconsistency; stopping FinalizationActor"
    );
    crate::errors::FinalizationError::SameHeightInconsistency {
        block_number,
        digest,
        finalized_hash: view.forkchoice.finalized_block_hash,
    }
    .into()
}

fn advance_last_finalized_round(view: &mut FinalizationView, round: Round) {
    view.last_finalized_round = Some(match view.last_finalized_round {
        Some(last_round) => std::cmp::max(last_round, round),
        None => round,
    });
}

fn advance_round_and_optional_seed(
    view: &mut FinalizationView,
    round: Round,
    vrf_seed: Option<alloy_primitives::B256>,
) {
    advance_last_finalized_round(view, round);
    if let Some(seed) = vrf_seed {
        view.prev_randao = seed;
    }
}

fn advance_finalized_view(
    view: &mut FinalizationView,
    round: Round,
    digest: Digest,
    block: &ConsensusBlock,
) {
    let block_number = block.number();
    advance_last_finalized_round(view, round);
    view.forkchoice.finalized_block_hash = digest.0;
    view.forkchoice.safe_block_hash = digest.0;
    view.forkchoice.head_block_hash = digest.0;
    view.last_finalized_number = block_number;
    view.last_timestamp_millis =
        std::cmp::max(view.last_timestamp_millis, block.timestamp_millis());
    outbe_tee::authority::note_finalized_authority_revision(block_number);
}

fn build_cn_record(
    witness: VerifiedChildParentWitness,
    digest: Digest,
) -> CertifiedParentProofRecord {
    CertifiedParentProofRecord {
        format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
        kind: ProofKind::CertifiedNotarization,
        finalized_epoch: witness.round.epoch().get(),
        finalized_view: witness.round.view().get(),
        parent_view: witness.parent_view,
        finalized_block_hash: digest.0,
        committee_set_hash: witness.metadata.committee_set_hash,
        vrf_material_version: witness.metadata.vrf_material_version,
        vrf_group_public_key_hash: witness.metadata.vrf_group_public_key_hash,
        ordered_committee: witness.metadata.ordered_committee,
        signer_bitmap: witness.metadata.signer_bitmap,
        encoded_proof: witness.metadata.proof,
    }
}
