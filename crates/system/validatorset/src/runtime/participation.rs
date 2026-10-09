use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::ValidatorLifecycle;
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::Mapping;

impl ValidatorSet<'_> {
    /// Records a block proposal by the given validator.
    ///
    /// Increments `blocks_proposed` for a current consensus participant.
    pub fn record_proposer(&mut self, addr: Address) -> Result<()> {
        if !self.is_consensus_participant(addr)? {
            return Err(PrecompileError::Revert(format!(
                "proposer is not a current consensus participant: {addr}"
            )));
        }
        increment_counter(&self.val_blocks_proposed, &addr, "blocks proposed overflow")
    }

    /// Records a missed block for the given validator.
    pub fn record_missed_block(&mut self, addr: Address) -> Result<()> {
        increment_counter(&self.val_missed_blocks, &addr, "missed blocks overflow")
    }

    /// Records vote participation: increments `missed_votes` for each absent validator.
    pub fn record_participation(&mut self, voters: &[Address], absent: &[Address]) -> Result<()> {
        self.record_vote_participation(voters, absent, VotingCommittee::Current)
    }

    /// Records vote participation for a historical (finalized-parent) committee.
    ///
    /// Finalized-parent metadata describes a committee captured at a previous
    /// finalized block. By the time this function applies it, some members may
    /// no longer be current consensus participants (e.g. transitioned to
    /// `UNBONDING` after a reshare). This entrypoint validates that every
    /// supplied address is a registered validator but does not require current
    /// `ACTIVE`/`EXITING` + `has_bls_share` membership.
    pub fn record_finalized_participation(
        &mut self,
        voters: &[Address],
        absent: &[Address],
    ) -> Result<()> {
        self.record_vote_participation(voters, absent, VotingCommittee::Finalized)
    }

    /// Requires every voter, then every absent voter, to belong to `committee`.
    /// Each absent voter is checked and then gets one more missed vote, in
    /// order.
    fn record_vote_participation(
        &mut self,
        voters: &[Address],
        absent: &[Address],
        committee: VotingCommittee,
    ) -> Result<()> {
        for addr in voters {
            if !self.is_committee_member(committee, *addr)? {
                return Err(PrecompileError::Revert(committee.voter_rejection(addr)));
            }
        }
        for addr in absent {
            if !self.is_committee_member(committee, *addr)? {
                return Err(PrecompileError::Revert(committee.absent_rejection(addr)));
            }
            increment_counter(&self.val_missed_votes, addr, "missed votes overflow")?;
        }
        Ok(())
    }

    fn is_committee_member(&self, committee: VotingCommittee, addr: Address) -> Result<bool> {
        match committee {
            VotingCommittee::Current => self.is_consensus_participant(addr),
            VotingCommittee::Finalized => self.is_validator(addr),
        }
    }

    /// Resets ValidatorSet-owned per-epoch counters for the outgoing committee.
    ///
    /// Kept separate from [`Self::advance_epoch`] because certified-parent and
    /// late-finalization accounting execute before the receipt-visible boundary
    /// activation. The executor resets only for a block that actually carries a
    /// certified BoundaryOutcome, then advances the epoch later in that block.
    pub fn reset_epoch_counters(&mut self) -> Result<()> {
        for addr in self.registered_validator_addresses()? {
            // Only reset counters for validators that accumulate them.
            // Include EXITING. They still participate in consensus
            // until reshare completes and accumulate per-epoch counters.
            // JailRetained is likewise still in the live committee until the
            // next reshare clears its share, so reset its counters too. Jail is
            // already excluded. Unjail clears late historical counters.
            if !matches!(
                self.validator_lifecycle(addr)?,
                ValidatorLifecycle::Active(_)
                    | ValidatorLifecycle::Exiting(_)
                    | ValidatorLifecycle::JailRetained(_)
            ) {
                continue;
            }
            self.val_missed_blocks.write(&addr, 0)?;
            self.val_missed_votes.write(&addr, 0)?;
            self.val_blocks_proposed.write(&addr, 0)?;
        }

        Ok(())
    }

    /// Advances the activated epoch anchor without resetting counters.
    pub fn advance_epoch(&mut self, timestamp: u64, block_number: u64) -> Result<()> {
        let epoch = self.epoch_number.read()?;
        let new_epoch = epoch
            .checked_add(U256::from(1))
            .ok_or_else(|| PrecompileError::Fatal("epoch number overflow".into()))?;
        self.epoch_number.write(new_epoch)?;
        self.epoch_start_timestamp.write(timestamp)?;
        self.epoch_start_block.write(block_number)?;

        let active_count = self.active_validator_count()?;
        self.emit(IValidatorSet::EpochTransition {
            newEpochNumber: new_epoch,
            timestamp,
            activeValidatorCount: active_count,
        })?;

        Ok(())
    }

    /// Resets per-epoch counters and advances the epoch.
    ///
    /// This combined form remains the module-level convenience API. Production
    /// boundary execution uses the two explicit halves to preserve the
    /// LateFinalizeCredits ordering contract.
    pub fn update_epoch(&mut self, timestamp: u64, block_number: u64) -> Result<()> {
        self.reset_epoch_counters()?;
        self.advance_epoch(timestamp, block_number)
    }
}

/// The committee that a vote-participation record describes.
#[derive(Clone, Copy)]
enum VotingCommittee {
    /// The current consensus participants.
    Current,
    /// A finalized-parent committee, whose members must still be registered.
    Finalized,
}

impl VotingCommittee {
    fn voter_rejection(self, addr: &Address) -> String {
        match self {
            Self::Current => format!("voter is not a current consensus participant: {addr}"),
            Self::Finalized => format!("finalized voter is not a registered validator: {addr}"),
        }
    }

    fn absent_rejection(self, addr: &Address) -> String {
        match self {
            Self::Current => {
                format!("absent voter is not a current consensus participant: {addr}")
            }
            Self::Finalized => {
                format!("finalized absent voter is not a registered validator: {addr}")
            }
        }
    }
}

/// Adds one to `counter[addr]`. An overflow fails with `overflow`.
fn increment_counter(
    counter: &Mapping<'_, Address, u64>,
    addr: &Address,
    overflow: &'static str,
) -> Result<()> {
    let value = counter.read(addr)?;
    counter.write(
        addr,
        value
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Fatal(overflow.into()))?,
    )
}
