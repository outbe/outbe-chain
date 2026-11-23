//! Reconciliation coordinator and catch-up job initialization for `FinalizationActor`.

use std::collections::BTreeSet;

use commonware_runtime::Clock;

use super::{
    archive_error, marshal_read, marshal_read_block, CatchUpJob, CatchUpPhase, FinalizationActor,
    MarshalRead, ParkedReason, ReconcileStepOutcome,
};
use crate::block::ConsensusBlock;
use crate::finalization::evidence::{
    check_missing_verifier_epoch, verify_direct_finalization, verify_live_finalized_cert,
    EventWorkBudget, VerifiedDirectFinalization,
};
use crate::finalization::ingress::Finalized;
use crate::marshal_types::MarshalMailbox;
use crate::marshal_update_reporter::ConsensusTip;

enum TargetPrepare {
    JobReady,
    Stop(ReconcileStepOutcome),
}

enum StartProofResult {
    Verified(Option<Box<VerifiedDirectFinalization>>),
    Parked(ParkedReason),
}

impl FinalizationActor {
    pub(super) async fn step_reconcile_with_clock<C: Clock>(
        &mut self,
        clock: &C,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        self.step_reconcile_with_optional_clock(Some(clock), budget)
            .await
    }

    #[cfg(test)]
    pub(super) async fn step_reconcile(
        &mut self,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        self.step_reconcile_with_optional_clock(
            None::<&commonware_runtime::deterministic::Context>,
            budget,
        )
        .await
    }

    async fn step_reconcile_with_optional_clock<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        self.parked_reason = None;
        let outcome = self.step_reconcile_inner(clock, budget).await;
        #[cfg(test)]
        {
            self.last_event_stats = budget.stats;
        }
        outcome
    }

    async fn step_reconcile_inner<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<ReconcileStepOutcome> {
        if let Some(outcome) = self.retry_head_proof(clock, budget).await? {
            return Ok(outcome);
        }
        self.retry_unresolved(clock, budget).await?;
        if budget.marshal_timed_out {
            self.parked_reason = Some(ParkedReason::MissingBlock);
            return Ok(ReconcileStepOutcome::Parked);
        }
        loop {
            if self.active_job.is_none() {
                if let TargetPrepare::Stop(outcome) = self.prepare_active_job(clock, budget).await?
                {
                    return Ok(outcome);
                }
            }
            match self.step_job(clock, budget).await? {
                ReconcileStepOutcome::Idle => continue,
                other => return Ok(other),
            }
        }
    }

    async fn prepare_active_job<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<TargetPrepare> {
        let (head_height, head_hash) = self.current_head();
        if self.parked_probe_still_blocks(head_height, head_hash) {
            self.parked_reason = Some(ParkedReason::MissingVerifier);
            return Ok(TargetPrepare::Stop(ReconcileStepOutcome::Parked));
        }
        if let Some(outcome) = self
            .drain_replayed_pending_target(clock, head_height, head_hash, budget)
            .await?
        {
            return Ok(TargetPrepare::Stop(outcome));
        }
        let Some(target) = self.pending_target.as_ref() else {
            let outcome = if self.unresolved.is_empty() && self.head_proof_retry.is_none() {
                ReconcileStepOutcome::Idle
            } else {
                ReconcileStepOutcome::Parked
            };
            return Ok(TargetPrepare::Stop(outcome));
        };
        let required_reads = if target.live.is_some() { 1 } else { 2 };
        if !budget.can_read_count(required_reads) || !budget.can_crypto() {
            return Ok(TargetPrepare::Stop(ReconcileStepOutcome::YieldMoreWork));
        }
        let tip = target.tip;
        let live = target.live.as_deref();
        let (job, reason) = self.start_job(clock, tip, live, budget).await?;
        let Some(job) = job else {
            self.parked_reason = reason;
            return Ok(TargetPrepare::Stop(ReconcileStepOutcome::Parked));
        };
        self.active_job = Some(job);
        Ok(TargetPrepare::JobReady)
    }

    fn parked_probe_still_blocks(
        &mut self,
        head_height: u64,
        head_hash: alloy_primitives::B256,
    ) -> bool {
        let Some(probe) = self.parked_probe.as_ref() else {
            return false;
        };
        let unchanged = self.pending_target.as_ref().is_some_and(|target| {
            target.tip == probe.target
                && head_height == probe.base_height
                && head_hash == probe.base_hash
        });
        let registered = probe
            .missing_epochs
            .iter()
            .copied()
            .any(|epoch| self.epoch_is_registered(epoch));
        if unchanged && !registered {
            return true;
        }
        self.parked_probe = None;
        false
    }

    async fn drain_replayed_pending_target<C: Clock>(
        &mut self,
        clock: Option<&C>,
        head_height: u64,
        head_hash: alloy_primitives::B256,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<ReconcileStepOutcome>> {
        let Some(tip) = self
            .pending_target
            .as_ref()
            .filter(|target| target.tip.height.get() <= head_height)
            .map(|target| target.tip)
        else {
            return Ok(None);
        };
        let Some(block) = self
            .load_replayed_target_block(clock, tip, head_height, head_hash, budget)
            .await?
        else {
            return Ok(Some(if self.parked_reason.is_some() {
                ReconcileStepOutcome::Parked
            } else {
                ReconcileStepOutcome::YieldMoreWork
            }));
        };
        if block.number() != tip.height.get() || block.digest() != tip.digest {
            return Err(archive_error(
                tip.height.get(),
                "replayed target differs from archived block",
            )
            .into());
        }
        if tip.height.get() == head_height {
            if let Some(live) = self
                .pending_target
                .as_mut()
                .and_then(|target| target.live.take())
            {
                self.retain_head_proof(*live);
            }
        }
        self.pending_target = None;
        Ok(None)
    }

    async fn load_replayed_target_block<C: Clock>(
        &mut self,
        clock: Option<&C>,
        tip: ConsensusTip,
        head_height: u64,
        head_hash: alloy_primitives::B256,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<ConsensusBlock>> {
        if tip.height.get() == head_height {
            if tip.digest.0 != head_hash {
                return Err(
                    archive_error(tip.height.get(), "replayed target differs from head").into(),
                );
            }
            if let Some(block) = self.deps.block_cache.get(&tip.digest) {
                return Ok(Some(block));
            }
        }
        if !budget.can_read() {
            return Ok(None);
        }
        let marshal = self.marshal()?;
        let _ = budget.charge_read();
        match marshal_read_block(clock, budget, marshal.get_block(tip.height)).await {
            MarshalRead::Found(block) => Ok(Some(block)),
            MarshalRead::Absent | MarshalRead::TimedOut => {
                self.parked_reason = Some(ParkedReason::MissingBlock);
                Ok(None)
            }
        }
    }

    async fn start_job<C: Clock>(
        &self,
        clock: Option<&C>,
        tip: ConsensusTip,
        live: Option<&Finalized>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<(Option<CatchUpJob>, Option<ParkedReason>)> {
        let marshal = self.marshal()?;
        let (base_height, base_hash) = self.current_head();
        if tip.height.get() <= base_height {
            return Ok((None, None));
        }
        let _ = budget.charge_read();
        let block = match marshal_read_block(clock, budget, marshal.get_block(tip.height)).await {
            MarshalRead::Found(block) => block,
            MarshalRead::Absent | MarshalRead::TimedOut => {
                return Ok((None, Some(ParkedReason::MissingBlock)));
            }
        };
        if block.number() != tip.height.get() || block.digest() != tip.digest {
            return Err(archive_error(
                tip.height.get(),
                "target differs from finalized archive block",
            )
            .into());
        }
        let verified = match self
            .verify_start_job_proof(clock, &marshal, tip, live, budget)
            .await?
        {
            StartProofResult::Verified(verified) => verified.map(|v| *v),
            StartProofResult::Parked(reason) => return Ok((None, Some(reason))),
        };
        let job = self.build_catch_up_job(base_height, base_hash, tip, verified)?;
        Ok((Some(job), None))
    }

    async fn verify_start_job_proof<C: Clock>(
        &self,
        clock: Option<&C>,
        marshal: &MarshalMailbox,
        tip: ConsensusTip,
        live: Option<&Finalized>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<StartProofResult> {
        if let Some(live) = live {
            let _ = budget.charge_crypto();
            let verified = verify_live_finalized_cert(
                tip.height.get(),
                live,
                &self.deps.certificate_scheme_provider,
                &self.deps.committee_provider,
            )?;
            return Ok(StartProofResult::Verified(verified.map(Box::new)));
        }
        let _ = budget.charge_read();
        let f = match marshal_read(clock, budget, marshal.get_finalization(tip.height)).await {
            MarshalRead::Found(f) => f,
            MarshalRead::Absent => {
                return Ok(StartProofResult::Parked(ParkedReason::MissingCertificate))
            }
            MarshalRead::TimedOut => {
                return Ok(StartProofResult::Parked(ParkedReason::MissingBlock))
            }
        };
        if f.proposal.payload != tip.digest || f.proposal.round != tip.round {
            return Err(archive_error(tip.height.get(), "target differs from archived F").into());
        }
        if !self.epoch_is_registered(tip.round.epoch()) {
            return Ok(StartProofResult::Verified(None));
        }
        let _ = budget.charge_crypto();
        let verified = verify_direct_finalization(
            tip.height.get(),
            &f,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?;
        Ok(StartProofResult::Verified(verified.map(Box::new)))
    }

    fn build_catch_up_job(
        &self,
        base_height: u64,
        base_hash: alloy_primitives::B256,
        tip: ConsensusTip,
        verified: Option<VerifiedDirectFinalization>,
    ) -> eyre::Result<CatchUpJob> {
        let mut missing_epochs = BTreeSet::new();
        let phase = if verified.is_some() {
            CatchUpPhase::PreflightAncestry {
                height: tip.height.get(),
                expected: tip.digest,
            }
        } else {
            let accepted_epoch = self
                .deps
                .view
                .read()
                .last_finalized_round
                .map(|r| r.epoch());
            let _ =
                check_missing_verifier_epoch(tip.height.get(), tip.round.epoch(), accepted_epoch)?;
            missing_epochs.insert(tip.round.epoch());
            CatchUpPhase::LocateAnchor {
                height: tip.height.get(),
                child: None,
            }
        };
        Ok(CatchUpJob {
            base_height,
            base_hash,
            anchor_height: tip.height.get(),
            anchor_digest: tip.digest,
            anchor_proof: verified,
            temporary_anchor: false,
            missing_epochs,
            phase,
        })
    }
}
