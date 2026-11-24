//! Unresolved finalization and head-proof retry logic for `FinalizationActor`.

use commonware_consensus::types::{Height, Round};
use commonware_runtime::Clock;

use super::{
    archive_error, canonicalize_live_finalized, marshal_read_block, ApplyOutcome,
    FinalizationActor, MarshalRead, ParkedReason, ReconcileStepOutcome,
};
use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::evidence::{
    check_missing_verifier_epoch, verify_live_finalized_cert, EventWorkBudget,
};
use crate::finalization::ingress::Finalized;
use crate::marshal_types::MarshalMailbox;
use crate::marshal_update_reporter::ConsensusTip;

enum UnresolvedEntryStep {
    Continue,
    TimedOut,
}

enum HeadBlockLoad {
    Loaded(ConsensusBlock),
    Outcome(Option<ReconcileStepOutcome>),
}

impl FinalizationActor {
    pub(super) async fn retry_unresolved<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        if self.unresolved.is_empty() {
            self.unresolved_cursor = None;
            return Ok(());
        }
        let keys = self.select_unresolved_batch();
        let marshal = self.marshal()?;
        for round in keys {
            if !budget.can_read_count(2) {
                break;
            }
            self.unresolved_cursor = Some(round);
            if matches!(
                self.retry_unresolved_entry(clock, &marshal, round, budget)
                    .await?,
                UnresolvedEntryStep::TimedOut
            ) {
                return Ok(());
            }
        }
        Ok(())
    }

    fn select_unresolved_batch(&self) -> Vec<Round> {
        let after = self.unresolved_cursor;
        let keys: Vec<Round> = self
            .unresolved
            .keys()
            .copied()
            .filter(|round| after.is_none_or(|last| *round > last))
            .take(16)
            .collect();
        if keys.is_empty() {
            self.unresolved.keys().copied().take(16).collect()
        } else {
            keys
        }
    }

    async fn retry_unresolved_entry<C: Clock>(
        &mut self,
        clock: Option<&C>,
        marshal: &MarshalMailbox,
        round: Round,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<UnresolvedEntryStep> {
        let digest = self.unresolved[&round];
        let _ = budget.charge_read();
        let block = match marshal_read_block(clock, budget, marshal.get_block(&digest)).await {
            MarshalRead::Found(block) => block,
            MarshalRead::Absent => return Ok(UnresolvedEntryStep::Continue),
            MarshalRead::TimedOut => return Ok(UnresolvedEntryStep::TimedOut),
        };
        let height = block.number();
        let (head_height, head_hash) = self.current_head();
        validate_unresolved_block(&block, digest, height, head_height, head_hash)?;
        let _ = budget.charge_read();
        match marshal_read_block(clock, budget, marshal.get_block(Height::new(height))).await {
            MarshalRead::Found(archived) => {
                if archived.number() != height || archived.digest() != digest {
                    return Err(archive_error(
                        height,
                        "unresolved voter F conflicts with finalized archive",
                    )
                    .into());
                }
            }
            MarshalRead::Absent if height == head_height && digest.0 == head_hash => {}
            MarshalRead::Absent => return Ok(UnresolvedEntryStep::Continue),
            MarshalRead::TimedOut => return Ok(UnresolvedEntryStep::TimedOut),
        }
        self.promote_resolved_unresolved(round, height, digest, head_height)?;
        Ok(UnresolvedEntryStep::Continue)
    }

    fn promote_resolved_unresolved(
        &mut self,
        round: Round,
        height: u64,
        digest: Digest,
        head_height: u64,
    ) -> eyre::Result<()> {
        if self
            .pending_target
            .as_ref()
            .is_some_and(|target| target.tip.height.get() == height && target.tip.digest != digest)
        {
            return Err(archive_error(height, "unresolved voter F conflicts with target").into());
        }
        if height > head_height {
            self.record_target(
                ConsensusTip {
                    round,
                    height: Height::new(height),
                    digest,
                },
                None,
            )?;
        }
        self.unresolved.remove(&round);
        Ok(())
    }

    pub(super) async fn retry_head_proof<C: Clock>(
        &mut self,
        clock: Option<&C>,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<ReconcileStepOutcome>> {
        let Some(proof) = self.head_proof_retry.take() else {
            return Ok(None);
        };
        let (height, head_hash) = self.current_head();
        if proof.digest.0 != head_hash {
            return Ok(None);
        }
        let higher_work = self.has_higher_work(height);
        let block = match self
            .load_head_proof_block(clock, proof, height, higher_work, budget)
            .await?
        {
            HeadBlockLoad::Loaded(block) => block,
            HeadBlockLoad::Outcome(outcome) => return Ok(outcome),
        };
        let Some(proof) = self.head_proof_retry.take() else {
            return Ok(None);
        };
        self.verify_and_apply_head_proof(proof, block, height, higher_work, budget)
            .await
    }

    fn has_higher_work(&self, head_height: u64) -> bool {
        self.pending_target
            .as_ref()
            .is_some_and(|target| target.tip.height.get() > head_height)
            || self.active_job.is_some()
            || !self.unresolved.is_empty()
    }

    fn park_head_proof(
        &mut self,
        proof: Box<Finalized>,
        higher_work: bool,
        reason: ParkedReason,
    ) -> Option<ReconcileStepOutcome> {
        self.head_proof_retry = Some(proof);
        if !higher_work {
            self.parked_reason = Some(reason);
        }
        (!higher_work).then_some(ReconcileStepOutcome::Parked)
    }

    async fn load_head_proof_block<C: Clock>(
        &mut self,
        clock: Option<&C>,
        proof: Box<Finalized>,
        height: u64,
        higher_work: bool,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<HeadBlockLoad> {
        let read = if let Some(block) = self.deps.block_cache.get(&proof.digest) {
            MarshalRead::Found(block)
        } else if budget.charge_read() {
            let marshal = self.marshal()?;
            marshal_read_block(clock, budget, marshal.get_block(Height::new(height))).await
        } else {
            self.head_proof_retry = Some(proof);
            return Ok(HeadBlockLoad::Outcome(Some(
                ReconcileStepOutcome::YieldMoreWork,
            )));
        };
        match read {
            MarshalRead::Found(block) => {
                if block.number() != height || block.digest() != proof.digest {
                    return Err(archive_error(
                        height,
                        "head proof block differs from accepted head",
                    )
                    .into());
                }
                self.head_proof_retry = Some(proof);
                Ok(HeadBlockLoad::Loaded(block))
            }
            MarshalRead::Absent => Ok(HeadBlockLoad::Outcome(self.park_head_proof(
                proof,
                higher_work,
                ParkedReason::MissingBlock,
            ))),
            MarshalRead::TimedOut => {
                if higher_work {
                    // The timed-out head proof is only a local handoff repair.
                    // Its timeout must not mark the independent higher target
                    // as timed out or stop its work in this event.
                    budget.marshal_timed_out = false;
                }
                Ok(HeadBlockLoad::Outcome(self.park_head_proof(
                    proof,
                    higher_work,
                    ParkedReason::MissingBlock,
                )))
            }
        }
    }

    async fn verify_and_apply_head_proof(
        &mut self,
        proof: Box<Finalized>,
        block: ConsensusBlock,
        height: u64,
        higher_work: bool,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<ReconcileStepOutcome>> {
        if !budget.charge_crypto() {
            self.head_proof_retry = Some(proof);
            return Ok(Some(ReconcileStepOutcome::YieldMoreWork));
        }
        let verified = verify_live_finalized_cert(
            height,
            &proof,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?;
        let Some(verified) = verified else {
            let accepted_epoch = self
                .deps
                .view
                .read()
                .last_finalized_round
                .map(|r| r.epoch());
            let _ = check_missing_verifier_epoch(height, proof.round.epoch(), accepted_epoch)?;
            return Ok(self.park_head_proof(proof, higher_work, ParkedReason::MissingVerifier));
        };
        let proof = canonicalize_live_finalized(*proof, &verified);
        let retry = Finalized {
            round: proof.round,
            digest: proof.digest,
            vrf_seed: proof.vrf_seed,
            consensus_data: proof.consensus_data.clone(),
        };
        match self.process_finalization(proof, block).await? {
            ApplyOutcome::Applied => Ok(None),
            ApplyOutcome::PendingPersist => {
                Ok(self.park_head_proof(Box::new(retry), higher_work, ParkedReason::Persistence))
            }
            ApplyOutcome::PendingVerifier => Ok(self.park_head_proof(
                Box::new(retry),
                higher_work,
                ParkedReason::MissingVerifier,
            )),
        }
    }
}

fn validate_unresolved_block(
    block: &ConsensusBlock,
    digest: Digest,
    height: u64,
    head_height: u64,
    head_hash: alloy_primitives::B256,
) -> eyre::Result<()> {
    if block.digest() != digest {
        return Err(archive_error(height, "unresolved voter block digest mismatch").into());
    }
    if height == head_height && digest.0 != head_hash {
        return Err(archive_error(height, "unresolved voter F conflicts with head").into());
    }
    Ok(())
}
