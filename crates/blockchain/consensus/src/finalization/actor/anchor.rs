//! Anchor discovery (`CatchUpPhase::LocateAnchor`) for `FinalizationActor`.

use commonware_consensus::types::{Epoch, Height};
use commonware_runtime::Clock;
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};

use super::catch_up::{BlockFetch, JobStepAction, StepCtx};
use super::{
    archive_error, marshal_read, CatchUpJob, CatchUpPhase, FinalizationActor, MarshalRead,
    ParkedProbe, ParkedReason, ReconcileStepOutcome,
};
use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::evidence::{
    certified_parent_metadata_from_phase1_system_tx, verify_child_parent_witness,
    verify_direct_finalization, EventWorkBudget, VerifiedDirectFinalization,
};

enum AnchorProofAttempt {
    Resolved(Option<Box<VerifiedDirectFinalization>>),
    Yield(ReconcileStepOutcome),
}

impl FinalizationActor {
    pub(super) async fn step_locate_anchor<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<JobStepAction> {
        let CatchUpPhase::LocateAnchor { height, child } = &job.phase else {
            unreachable!();
        };
        let height = *height;
        if height <= job.base_height {
            return Ok(self.finish_anchor_search_missing_verifier(job));
        }
        let block = match self.read_job_block(ctx, height, budget).await {
            BlockFetch::Loaded(block) => block,
            BlockFetch::Yield(outcome) => return Ok(JobStepAction::Yield(outcome)),
        };
        validate_anchor_candidate(&block, height, child.as_ref(), job.anchor_digest)?;
        let verified = match self
            .resolve_anchor_proof(ctx, height, &block, job, budget)
            .await?
        {
            AnchorProofAttempt::Resolved(verified) => verified.map(|v| *v),
            AnchorProofAttempt::Yield(outcome) => return Ok(JobStepAction::Yield(outcome)),
        };
        self.advance_locate_anchor(job, height, block, verified);
        Ok(JobStepAction::Continue)
    }

    async fn resolve_anchor_proof<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        height: u64,
        block: &ConsensusBlock,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<AnchorProofAttempt> {
        match self
            .verify_anchor_archive_finalization(ctx, block, job, budget)
            .await?
        {
            AnchorProofAttempt::Resolved(None) => {
                self.verify_anchor_embedded_finalization(height, block, job, budget)
            }
            other => Ok(other),
        }
    }

    fn finish_anchor_search_missing_verifier(&mut self, job: &mut CatchUpJob) -> JobStepAction {
        self.parked_reason = Some(ParkedReason::MissingVerifier);
        if let Some(target) = self.pending_target.as_ref().map(|target| target.tip) {
            self.parked_probe = Some(ParkedProbe {
                target,
                base_height: job.base_height,
                base_hash: job.base_hash,
                missing_epochs: std::mem::take(&mut job.missing_epochs),
            });
            return JobStepAction::Finish(ReconcileStepOutcome::Parked);
        }
        JobStepAction::Finish(ReconcileStepOutcome::Idle)
    }

    async fn verify_anchor_archive_finalization<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        block: &ConsensusBlock,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<AnchorProofAttempt> {
        let height = block.number();
        if !budget.can_read() {
            return Ok(AnchorProofAttempt::Yield(
                ReconcileStepOutcome::YieldMoreWork,
            ));
        }
        let _ = budget.charge_read();
        let archive_f = match marshal_read(
            ctx.clock,
            budget,
            ctx.marshal.get_finalization(Height::new(height)),
        )
        .await
        {
            MarshalRead::Found(f) => f,
            MarshalRead::Absent => return Ok(AnchorProofAttempt::Resolved(None)),
            MarshalRead::TimedOut => {
                self.parked_reason = Some(ParkedReason::MissingBlock);
                return Ok(AnchorProofAttempt::Yield(ReconcileStepOutcome::Parked));
            }
        };
        if archive_f.proposal.payload != block.digest() {
            return Err(archive_error(height, "anchor F payload differs from branch block").into());
        }
        self.verify_found_archive_finalization(height, &archive_f, job, budget)
    }

    fn verify_found_archive_finalization(
        &self,
        height: u64,
        archive_f: &commonware_consensus::simplex::types::Finalization<
            crate::hybrid::HybridScheme<
                commonware_cryptography::bls12381::primitives::variant::MinSig,
            >,
            Digest,
        >,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<AnchorProofAttempt> {
        let epoch = archive_f.proposal.round.epoch();
        if !self.epoch_is_registered(epoch) {
            job.missing_epochs.insert(epoch);
            return Ok(AnchorProofAttempt::Resolved(None));
        }
        if !budget.can_crypto() {
            return Ok(AnchorProofAttempt::Yield(
                ReconcileStepOutcome::YieldMoreWork,
            ));
        }
        let _ = budget.charge_crypto();
        let verified = verify_direct_finalization(
            height,
            archive_f,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?;
        Ok(AnchorProofAttempt::Resolved(verified.map(Box::new)))
    }

    fn verify_anchor_embedded_finalization(
        &self,
        height: u64,
        block: &ConsensusBlock,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<AnchorProofAttempt> {
        let CatchUpPhase::LocateAnchor {
            child: Some(upper), ..
        } = &job.phase
        else {
            return Ok(AnchorProofAttempt::Resolved(None));
        };
        let Some(metadata) = extract_embedded_finalization_metadata(upper, height, block)? else {
            return Ok(AnchorProofAttempt::Resolved(None));
        };
        let candidate_epoch = Epoch::new(metadata.finalized_epoch);
        if !self.epoch_is_registered(candidate_epoch) {
            job.missing_epochs.insert(candidate_epoch);
            return Ok(AnchorProofAttempt::Resolved(None));
        }
        if !budget.can_crypto_count(2) {
            return Ok(AnchorProofAttempt::Yield(
                ReconcileStepOutcome::YieldMoreWork,
            ));
        }
        let verified = self.verify_embedded_witness_finalization(height, block, upper, budget)?;
        Ok(AnchorProofAttempt::Resolved(verified.map(Box::new)))
    }

    fn verify_embedded_witness_finalization(
        &self,
        height: u64,
        block: &ConsensusBlock,
        upper: &ConsensusBlock,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<VerifiedDirectFinalization>> {
        let _ = budget.charge_crypto();
        let witness = verify_child_parent_witness(
            block,
            upper,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?
        .ok_or_else(|| archive_error(height, "embedded anchor verifier vanished"))?;
        let f = witness
            .finalization
            .ok_or_else(|| archive_error(height, "embedded anchor has no F"))?;
        let _ = budget.charge_crypto();
        verify_direct_finalization(
            height,
            &f,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )
    }

    fn advance_locate_anchor(
        &self,
        job: &mut CatchUpJob,
        height: u64,
        block: ConsensusBlock,
        verified: Option<VerifiedDirectFinalization>,
    ) {
        if let Some(verified) = verified {
            job.anchor_height = height;
            job.anchor_digest = block.digest();
            job.anchor_proof = Some(verified);
            job.temporary_anchor = self
                .pending_target
                .as_ref()
                .is_some_and(|target| target.tip.height.get() > height);
            job.phase = CatchUpPhase::PreflightAncestry {
                height,
                expected: block.digest(),
            };
        } else {
            job.phase = CatchUpPhase::LocateAnchor {
                height: height - 1,
                child: Some(block),
            };
        }
    }
}

fn extract_embedded_finalization_metadata(
    upper: &ConsensusBlock,
    height: u64,
    block: &ConsensusBlock,
) -> eyre::Result<Option<CertifiedParentAccountingMetadata>> {
    let Some(metadata) = certified_parent_metadata_from_phase1_system_tx(upper)? else {
        return Ok(None);
    };
    if metadata.finalized_block_number != height
        || metadata.finalized_block_hash != block.digest().0
    {
        return Err(archive_error(height, "embedded F parent binding mismatch").into());
    }
    Ok((metadata.proof_kind == ParentParticipationProof::Finalization).then_some(metadata))
}

fn validate_anchor_candidate(
    block: &ConsensusBlock,
    height: u64,
    child: Option<&ConsensusBlock>,
    anchor_digest: Digest,
) -> eyre::Result<()> {
    if block.number() != height {
        return Err(archive_error(height, "anchor search height mismatch").into());
    }
    if let Some(upper) = child {
        if upper.parent_hash() != block.digest().0 {
            return Err(archive_error(height, "anchor search branch mismatch").into());
        }
    } else if block.digest() != anchor_digest {
        return Err(archive_error(height, "anchor search target digest mismatch").into());
    }
    Ok(())
}
