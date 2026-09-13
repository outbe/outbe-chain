use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::ValidatorLifecycle;
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

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
        let proposed = self.val_blocks_proposed.read(&addr)?;
        self.val_blocks_proposed.write(
            &addr,
            proposed
                .checked_add(1)
                .ok_or_else(|| PrecompileError::Fatal("blocks proposed overflow".into()))?,
        )?;

        Ok(())
    }

    /// Records a missed block for the given validator.
    pub fn record_missed_block(&mut self, addr: Address) -> Result<()> {
        let missed = self.val_missed_blocks.read(&addr)?;
        self.val_missed_blocks.write(
            &addr,
            missed
                .checked_add(1)
                .ok_or_else(|| PrecompileError::Fatal("missed blocks overflow".into()))?,
        )?;
        Ok(())
    }

    /// Records vote participation: increments `missed_votes` for each absent validator.
    pub fn record_participation(&mut self, voters: &[Address], absent: &[Address]) -> Result<()> {
        for addr in voters {
            if !self.is_consensus_participant(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "voter is not a current consensus participant: {addr}"
                )));
            }
        }
        for addr in absent {
            if !self.is_consensus_participant(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "absent voter is not a current consensus participant: {addr}"
                )));
            }
            let missed = self.val_missed_votes.read(addr)?;
            self.val_missed_votes.write(
                addr,
                missed
                    .checked_add(1)
                    .ok_or_else(|| PrecompileError::Fatal("missed votes overflow".into()))?,
            )?;
        }
        Ok(())
    }

    /// Records vote participation for a historical (finalized-parent) committee.
    ///
    /// Finalized-parent metadata describes a committee captured at a previous
    /// finalized block. By the time it is applied here, some members may no
    /// longer be current consensus participants (e.g. transitioned to
    /// `UNBONDING` after a reshare). This entrypoint validates that every
    /// supplied address is a registered validator but does not require current
    /// `ACTIVE`/`EXITING` + `has_bls_share` membership.
    pub fn record_finalized_participation(
        &mut self,
        voters: &[Address],
        absent: &[Address],
    ) -> Result<()> {
        for addr in voters {
            if !self.is_validator(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "finalized voter is not a registered validator: {addr}"
                )));
            }
        }
        for addr in absent {
            if !self.is_validator(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "finalized absent voter is not a registered validator: {addr}"
                )));
            }
            let missed = self.val_missed_votes.read(addr)?;
            self.val_missed_votes.write(
                addr,
                missed
                    .checked_add(1)
                    .ok_or_else(|| PrecompileError::Fatal("missed votes overflow".into()))?,
            )?;
        }
        Ok(())
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
            // Include EXITING - they still participate in consensus
            // until reshare completes and accumulate per-epoch counters.
            // JailRetained is likewise still in the live committee until the
            // next reshare clears its share, so reset its counters too. Jail is
            // already excluded; late historical counters are cleared on unjail.
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
