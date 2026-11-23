//! Live finalization and marshal tip ingestion for `FinalizationActor`.

use commonware_consensus::types::{Height, Round};
use commonware_runtime::Clock;

use super::{
    archive_error, canonicalize_live_finalized, marshal_read, marshal_read_block, ApplyOutcome,
    FinalizationActor, MarshalRead, ResolutionStall,
};
use crate::block::ConsensusBlock;
use crate::digest::Digest;
use crate::finalization::evidence::{
    check_missing_verifier_epoch, verify_live_finalized_cert, EventWorkBudget,
};
use crate::finalization::ingress::Finalized;
use crate::marshal_update_reporter::ConsensusTip;

enum LiveArchiveStatus {
    Proceed,
    RetainUnresolved,
}

impl FinalizationActor {
    pub(super) async fn handle_finalized_event<C: Clock>(
        &mut self,
        clock: Option<&C>,
        finalized: Finalized,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        let round = finalized.round;
        let digest = finalized.digest;
        let (head_height, head_hash) = self.current_head();
        self.clear_stale_head_proof(head_hash);
        self.check_round_identity(round, digest)?;

        if head_height > 0 && digest.0 == head_hash {
            self.retain_head_proof(finalized);
            return Ok(());
        }

        self.clear_mismatched_parked_probe(round, digest);
        let Some(block) = self
            .resolve_live_block(clock, round, digest, budget)
            .await?
        else {
            return self.retain_unresolved(round, digest);
        };
        if !budget.charge_crypto() {
            return self.retain_unresolved(round, digest);
        }
        let (finalized, verified) = self.verify_live_event_cert(finalized, &block)?;
        self.accept_resolved_live(clock, finalized, block, verified, budget)
            .await
    }

    fn clear_stale_head_proof(&mut self, head_hash: alloy_primitives::B256) {
        if self
            .head_proof_retry
            .as_ref()
            .is_some_and(|old| old.digest.0 != head_hash)
        {
            self.head_proof_retry = None;
        }
    }

    fn clear_mismatched_parked_probe(&mut self, round: Round, digest: Digest) {
        if self
            .parked_probe
            .as_ref()
            .is_some_and(|probe| probe.target.round != round || probe.target.digest != digest)
        {
            self.parked_probe = None;
        }
    }

    async fn resolve_live_block<C: Clock>(
        &mut self,
        clock: Option<&C>,
        round: Round,
        digest: Digest,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<Option<ConsensusBlock>> {
        if let Some(block) = self.deps.block_cache.get(&digest) {
            return Ok(Some(block));
        }
        self.retain_unresolved(round, digest)?;
        if let Some(clock) = clock {
            self.resolution_stall.get_or_insert(ResolutionStall {
                started: clock.current(),
                cycles: 0,
                alarmed: false,
            });
        }
        if !budget.charge_read() {
            return Ok(None);
        }
        let marshal = self.marshal()?;
        Ok(
            match marshal_read_block(clock, budget, marshal.get_block(&digest)).await {
                MarshalRead::Found(block) => Some(block),
                MarshalRead::Absent | MarshalRead::TimedOut => None,
            },
        )
    }

    fn verify_live_event_cert(
        &self,
        finalized: Finalized,
        block: &ConsensusBlock,
    ) -> eyre::Result<(Finalized, bool)> {
        let verified = verify_live_finalized_cert(
            block.number(),
            &finalized,
            &self.deps.certificate_scheme_provider,
            &self.deps.committee_provider,
        )?;
        if verified.is_none() {
            let accepted_epoch = self
                .deps
                .view
                .read()
                .last_finalized_round
                .map(|r| r.epoch());
            let _ = check_missing_verifier_epoch(
                block.number(),
                finalized.round.epoch(),
                accepted_epoch,
            )?;
        }
        let is_verified = verified.is_some();
        let finalized = match verified.as_ref() {
            Some(verified) => canonicalize_live_finalized(finalized, verified),
            None => finalized,
        };
        Ok((finalized, is_verified))
    }

    #[cfg(test)]
    pub(super) async fn handle_finalized(
        &mut self,
        clock: &impl Clock,
        finalized: Finalized,
    ) -> eyre::Result<()> {
        let mut budget = EventWorkBudget::new();
        self.handle_finalized_event(Some(clock), finalized, &mut budget)
            .await?;
        let _ = self.step_reconcile_with_clock(clock, &mut budget).await?;
        Ok(())
    }

    async fn accept_resolved_live<C: Clock>(
        &mut self,
        clock: Option<&C>,
        finalized: Finalized,
        block: ConsensusBlock,
        verified: bool,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        let height = block.number();
        let round = finalized.round;
        let digest = finalized.digest;
        if block.digest() != digest {
            return Err(archive_error(height, "resolved voter block digest differs from F").into());
        }
        let (head_height, head_hash) = self.current_head();
        if height <= head_height {
            return self
                .accept_historical_or_head_live(
                    clock,
                    finalized,
                    height,
                    (head_height, head_hash),
                    budget,
                )
                .await;
        }
        let allow_unarchived_direct =
            verified && height == head_height.saturating_add(1) && self.active_job.is_none();
        match self
            .check_live_archive_conflict(clock, height, digest, allow_unarchived_direct, budget)
            .await?
        {
            LiveArchiveStatus::RetainUnresolved => return self.retain_unresolved(round, digest),
            LiveArchiveStatus::Proceed => {}
        }
        self.validate_live_future_target(height, digest, &block, head_height, head_hash)?;
        if allow_unarchived_direct {
            return self.apply_direct_next_live(finalized, block, height).await;
        }
        self.record_target(
            ConsensusTip {
                round: finalized.round,
                height: Height::new(height),
                digest,
            },
            Some(Box::new(finalized)),
        )?;
        self.unresolved.remove(&round);
        Ok(())
    }

    async fn accept_historical_or_head_live<C: Clock>(
        &mut self,
        clock: Option<&C>,
        finalized: Finalized,
        height: u64,
        (head_height, head_hash): (u64, alloy_primitives::B256),
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        let round = finalized.round;
        let digest = finalized.digest;
        if height == head_height {
            if digest.0 != head_hash {
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: height,
                    digest,
                    finalized_hash: head_hash,
                }
                .into());
            }
            self.retain_head_proof(finalized);
            self.unresolved.remove(&round);
            return Ok(());
        }
        if !budget.charge_read() {
            return self.retain_unresolved(round, digest);
        }
        let marshal = self.marshal()?;
        let archived =
            marshal_read_block(clock, budget, marshal.get_block(Height::new(height))).await;
        let MarshalRead::Found(archived) = archived else {
            return self.retain_unresolved(round, digest);
        };
        if archived.digest() != digest {
            return Err(archive_error(
                height,
                "historical voter F conflicts with finalized archive",
            )
            .into());
        }
        self.unresolved.remove(&round);
        Ok(())
    }

    async fn check_live_archive_conflict<C: Clock>(
        &self,
        clock: Option<&C>,
        height: u64,
        digest: Digest,
        allow_absent: bool,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<LiveArchiveStatus> {
        if !budget.charge_read() {
            return Ok(LiveArchiveStatus::RetainUnresolved);
        }
        let marshal = self.marshal()?;
        match marshal_read_block(clock, budget, marshal.get_block(Height::new(height))).await {
            MarshalRead::Found(archived) if archived.digest() != digest => {
                Err(archive_error(height, "voter F conflicts with finalized archive").into())
            }
            MarshalRead::Absent if allow_absent => Ok(LiveArchiveStatus::Proceed),
            MarshalRead::Absent | MarshalRead::TimedOut => Ok(LiveArchiveStatus::RetainUnresolved),
            MarshalRead::Found(_) => Ok(LiveArchiveStatus::Proceed),
        }
    }

    fn validate_live_future_target(
        &self,
        height: u64,
        digest: Digest,
        block: &ConsensusBlock,
        head_height: u64,
        head_hash: alloy_primitives::B256,
    ) -> eyre::Result<()> {
        if self
            .pending_target
            .as_ref()
            .is_some_and(|target| target.tip.height.get() == height && target.tip.digest != digest)
        {
            return Err(archive_error(height, "voter F conflicts with same-height target").into());
        }
        if height == head_height.saturating_add(1) && block.parent_hash() != head_hash {
            return Err(
                archive_error(height, "voter F block does not extend accepted head").into(),
            );
        }
        Ok(())
    }

    async fn apply_direct_next_live(
        &mut self,
        finalized: Finalized,
        block: ConsensusBlock,
        height: u64,
    ) -> eyre::Result<()> {
        let retry_finalized = Finalized {
            round: finalized.round,
            digest: finalized.digest,
            vrf_seed: finalized.vrf_seed,
            consensus_data: finalized.consensus_data.clone(),
        };
        match self.process_finalization(finalized, block).await? {
            ApplyOutcome::Applied => {
                self.unresolved.remove(&retry_finalized.round);
                Ok(())
            }
            ApplyOutcome::PendingPersist | ApplyOutcome::PendingVerifier => self.record_target(
                ConsensusTip {
                    round: retry_finalized.round,
                    height: Height::new(height),
                    digest: retry_finalized.digest,
                },
                Some(Box::new(retry_finalized)),
            ),
        }
    }

    pub(super) async fn handle_tip<C: Clock>(
        &mut self,
        clock: Option<&C>,
        tip: ConsensusTip,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<()> {
        if self.check_tip_preconditions(tip)? {
            return Ok(());
        }
        let (head_height, _) = self.current_head();
        if !self
            .validate_tip_against_archive(clock, tip, budget)
            .await?
        {
            return self.record_target(tip, None);
        }
        if tip.height.get() <= head_height {
            return Ok(());
        }
        self.parked_probe = None;
        self.record_target(tip, None)
    }

    /// Returns `Ok(true)` if `tip` is an exact duplicate of the current pending target.
    fn check_tip_preconditions(&self, tip: ConsensusTip) -> eyre::Result<bool> {
        self.check_round_identity(tip.round, tip.digest)?;
        if let Some(current) = &self.pending_target {
            if current.tip.height == tip.height && current.tip.digest != tip.digest {
                return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: tip.height.get(),
                    digest: tip.digest,
                    finalized_hash: current.tip.digest.0,
                }
                .into());
            }
            if current.tip == tip {
                return Ok(true);
            }
        }
        let (head_height, head_hash) = self.current_head();
        if tip.height.get() == head_height && tip.digest.0 != head_hash {
            return Err(crate::errors::FinalizationError::SameHeightInconsistency {
                block_number: head_height,
                digest: tip.digest,
                finalized_hash: head_hash,
            }
            .into());
        }
        Ok(false)
    }

    /// Returns `Ok(true)` when archive reads completed (found or absent), or
    /// `Ok(false)` when budget was exhausted or a read timed out.
    async fn validate_tip_against_archive<C: Clock>(
        &self,
        clock: Option<&C>,
        tip: ConsensusTip,
        budget: &mut EventWorkBudget,
    ) -> eyre::Result<bool> {
        if !budget.charge_read() {
            return Ok(false);
        }
        let marshal = self.marshal()?;
        match marshal_read(clock, budget, marshal.get_finalization(tip.height)).await {
            MarshalRead::TimedOut => return Ok(false),
            MarshalRead::Found(f)
                if f.proposal.payload != tip.digest || f.proposal.round != tip.round =>
            {
                return Err(
                    archive_error(tip.height.get(), "marshal tip differs from archived F").into(),
                );
            }
            MarshalRead::Found(_) | MarshalRead::Absent => {}
        }
        if !budget.charge_read() {
            return Ok(false);
        }
        match marshal_read_block(clock, budget, marshal.get_block(tip.height)).await {
            MarshalRead::TimedOut => Ok(false),
            MarshalRead::Found(block)
                if block.number() != tip.height.get() || block.digest() != tip.digest =>
            {
                Err(
                    archive_error(tip.height.get(), "marshal tip differs from archived block")
                        .into(),
                )
            }
            MarshalRead::Found(_) | MarshalRead::Absent => Ok(true),
        }
    }

    #[cfg(test)]
    pub(super) async fn reconcile_marshal_tip(
        &mut self,
        tip: ConsensusTip,
    ) -> eyre::Result<super::ReconcileStepOutcome> {
        let mut budget = EventWorkBudget::new();
        self.handle_tip(
            None::<&commonware_runtime::deterministic::Context>,
            tip,
            &mut budget,
        )
        .await?;
        self.step_reconcile(&mut budget).await
    }
}
