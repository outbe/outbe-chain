//! Multi-step catch-up state machine (`PreflightAncestry` and `ApplyForward`).

use commonware_consensus::types::Height;
use commonware_runtime::Clock;
use outbe_primitives::consensus_metadata::ParentParticipationProof;

use super::{
    archive_error, marshal_read_block, ApplyOutcome, CatchUpJob, CatchUpPhase, FinalizationActor,
    MarshalRead, ParkedReason, ReconcileStepOutcome,
};
use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::evidence::{verify_child_parent_witness, EventWorkBudget, VerifiedStep};
use crate::marshal_types::MarshalMailbox;

pub(super) struct StepCtx<'a, C> {
    pub(super) clock: Option<&'a C>,
    pub(super) marshal: &'a MarshalMailbox,
}

impl<C> Clone for StepCtx<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for StepCtx<'_, C> {}

pub(super) enum JobStepAction {
    Continue,
    Yield(ReconcileStepOutcome),
    Finish(ReconcileStepOutcome),
}

pub(super) enum BlockFetch {
    Loaded(ConsensusBlock),
    Yield(ReconcileStepOutcome),
}

enum ForwardStepLoad {
    Loaded(Box<VerifiedStep>),
    Yield(ReconcileStepOutcome),
}

impl FinalizationActor {
    pub(super) async fn step_job<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        let Some(mut job) = self.active_job.take() else {
            return Ok(ReconcileStepOutcome::Idle);
        };
        let marshal = self.marshal()?;
        let ctx = StepCtx {
            clock,
            marshal: &marshal,
        };
        loop {
            let action = match job.phase {
                CatchUpPhase::LocateAnchor { .. } => {
                    self.step_locate_anchor(ctx, &mut job, budget).await?
                }
                CatchUpPhase::PreflightAncestry { .. } => {
                    self.step_preflight_ancestry(ctx, &mut job, budget).await?
                }
                CatchUpPhase::ApplyForward { .. } => {
                    self.step_apply_forward(ctx, &mut job, budget).await?
                }
            };
            match action {
                JobStepAction::Continue => continue,
                JobStepAction::Yield(outcome) => {
                    self.active_job = Some(job);
                    return Ok(outcome);
                }
                JobStepAction::Finish(outcome) => return Ok(outcome),
            }
        }
    }

    pub(super) async fn read_job_block<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        height: u64,
        budget: &mut EventWorkBudget,
    ) -> BlockFetch {
        if !budget.can_read() {
            return BlockFetch::Yield(ReconcileStepOutcome::YieldMoreWork);
        }
        let _ = budget.charge_read();
        match marshal_read_block(
            ctx.clock,
            budget,
            ctx.marshal.get_block(Height::new(height)),
        )
        .await
        {
            MarshalRead::Found(block) => BlockFetch::Loaded(block),
            MarshalRead::Absent | MarshalRead::TimedOut => {
                self.parked_reason = Some(ParkedReason::MissingBlock);
                BlockFetch::Yield(ReconcileStepOutcome::Parked)
            }
        }
    }

    async fn step_preflight_ancestry<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<JobStepAction> {
        let CatchUpPhase::PreflightAncestry { height, expected } = job.phase else {
            unreachable!();
        };
        let block = match self.read_job_block(ctx, height, budget).await {
            BlockFetch::Loaded(block) => block,
            BlockFetch::Yield(outcome) => return Ok(JobStepAction::Yield(outcome)),
        };
        if block.number() != height || block.digest() != expected {
            return Err(archive_error(height, "preflight ancestry block mismatch").into());
        }
        let parent = block.parent_hash();
        if height == job.base_height.saturating_add(1) {
            if parent != job.base_hash {
                return Err(archive_error(height, "preflight ancestry does not reach head").into());
            }
            job.phase = CatchUpPhase::ApplyForward {
                next_height: job.base_height.saturating_add(1),
                parent_hash: job.base_hash,
            };
        } else {
            job.phase = CatchUpPhase::PreflightAncestry {
                height: height - 1,
                expected: Digest(parent),
            };
        }
        Ok(JobStepAction::Continue)
    }

    async fn step_apply_forward<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        job: &mut CatchUpJob,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<JobStepAction> {
        let CatchUpPhase::ApplyForward {
            next_height: height,
            parent_hash,
        } = job.phase
        else {
            unreachable!();
        };
        if !budget.can_apply() || !budget.can_read() || !budget.can_crypto() {
            return Ok(JobStepAction::Yield(ReconcileStepOutcome::YieldMoreWork));
        }
        let block = match self.read_job_block(ctx, height, budget).await {
            BlockFetch::Loaded(block) => block,
            BlockFetch::Yield(outcome) => return Ok(JobStepAction::Yield(outcome)),
        };
        validate_forward_block(&block, height, parent_hash, job)?;
        let step = match self.resolve_forward_step(ctx, job, &block, budget).await? {
            ForwardStepLoad::Loaded(step) => *step,
            ForwardStepLoad::Yield(outcome) => return Ok(JobStepAction::Yield(outcome)),
        };
        self.commit_forward_step(job, parent_hash, step, &block, budget)
            .await
    }

    async fn commit_forward_step(
        &mut self,
        job: &mut CatchUpJob,
        parent_hash: alloy_primitives::B256,
        step: VerifiedStep,
        block: &ConsensusBlock,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<JobStepAction> {
        let height = block.number();
        let (head_height, head_hash) = self.current_head();
        if head_height.saturating_add(1) != height || head_hash != parent_hash {
            return Err(archive_error(height, "forward apply head changed").into());
        }
        if let Some(outcome) = self.apply_forward_step(step, block).await? {
            return Ok(JobStepAction::Yield(outcome));
        }
        let _ = budget.charge_apply();
        job.phase = CatchUpPhase::ApplyForward {
            next_height: height + 1,
            parent_hash: block.digest().0,
        };
        Ok(if height == job.anchor_height {
            JobStepAction::Finish(ReconcileStepOutcome::Idle)
        } else {
            JobStepAction::Continue
        })
    }

    async fn resolve_forward_step<C: Clock>(
        &mut self,
        ctx: StepCtx<'_, C>,
        job: &CatchUpJob,
        block: &ConsensusBlock,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ForwardStepLoad> {
        let height = block.number();
        if height == job.anchor_height && !job.temporary_anchor {
            let verified = job.anchor_proof.clone().expect("verified top F");
            return Ok(ForwardStepLoad::Loaded(Box::new(
                VerifiedStep::from_direct_f(height, verified),
            )));
        }
        let child = match self.read_job_block(ctx, height + 1, budget).await {
            BlockFetch::Loaded(child) => child,
            BlockFetch::Yield(outcome) => return Ok(ForwardStepLoad::Yield(outcome)),
        };
        let _ = budget.charge_crypto();
        let witness = verify_child_parent_witness(
            block,
            &child,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?;
        let Some(witness) = witness else {
            self.parked_reason = Some(ParkedReason::MissingVerifier);
            return Ok(ForwardStepLoad::Yield(ReconcileStepOutcome::Parked));
        };
        Ok(ForwardStepLoad::Loaded(Box::new(
            VerifiedStep::from_child_witness(height, block.digest(), witness),
        )))
    }

    async fn apply_forward_step(
        &mut self,
        step: VerifiedStep,
        block: &ConsensusBlock,
    ) -> eyre::Result<Option<ReconcileStepOutcome>> {
        let apply = match step.proof_kind {
            ParentParticipationProof::Finalization => {
                self.process_finalization(step.to_finalized(), block.clone())
                    .await?
            }
            ParentParticipationProof::CertifiedNotarization => {
                self.apply_implicit_cn(step.to_cn_witness(), block.clone())
                    .await?
            }
        };
        match apply {
            ApplyOutcome::Applied => Ok(None),
            ApplyOutcome::PendingPersist => {
                self.parked_reason = Some(ParkedReason::Persistence);
                Ok(Some(ReconcileStepOutcome::Parked))
            }
            ApplyOutcome::PendingVerifier => {
                self.parked_reason = Some(ParkedReason::MissingVerifier);
                Ok(Some(ReconcileStepOutcome::Parked))
            }
        }
    }
}

fn validate_forward_block(
    block: &ConsensusBlock,
    height: u64,
    parent_hash: alloy_primitives::B256,
    job: &CatchUpJob,
) -> eyre::Result<()> {
    if block.number() != height || block.parent_hash() != parent_hash {
        return Err(archive_error(
            height,
            "forward archive block does not extend accepted chain",
        )
        .into());
    }
    if height == job.anchor_height && block.digest() != job.anchor_digest {
        return Err(archive_error(height, "forward archive anchor changed").into());
    }
    Ok(())
}
