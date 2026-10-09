//! The unbonding queue: entries that unstaked or retired funds wait in until
//! their completion time.

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::{ValidatorLifecycle, ValidatorState};

use crate::contract::Staking;

impl Staking<'_> {
    pub(crate) fn checked_complete_time(&self, timestamp: u64, period: u64) -> Result<u64> {
        timestamp.checked_add(period).ok_or_else(|| {
            PrecompileError::Revert("unbonding completion timestamp overflow".into())
        })
    }

    pub(crate) fn slashed_withdrawal_delay(&self) -> Result<u64> {
        let configured = self.config_slashed_withdrawal_delay.read()?;
        if configured > 0 {
            return Ok(configured);
        }
        let unbonding_period = self.config_unbonding_period.read()?;
        unbonding_period
            .checked_mul(2)
            .ok_or_else(|| PrecompileError::Revert("slashed withdrawal delay overflow".into()))
    }

    pub(crate) fn enqueue_unbonding(
        &mut self,
        validator: Address,
        amount: U256,
        complete_time: u64,
    ) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let idx = self.unbonding_count.read()?;
        self.unbonding_validator.write(&idx, validator)?;
        self.unbonding_amount.write(&idx, amount)?;
        self.unbonding_complete_time.write(&idx, complete_time)?;
        self.unbonding_count.write(idx + 1)?;

        let prev_head_stored = self.per_val_unbonding_head.read(&validator)?;
        self.unbonding_next.write(&idx, prev_head_stored)?;
        self.per_val_unbonding_head.write(&validator, idx + 1)?;

        Ok(())
    }

    fn has_pending_unbonding(&self, validator: Address) -> Result<bool> {
        let mut current_stored = self.per_val_unbonding_head.read(&validator)?;
        while current_stored != 0 {
            let idx = current_stored - 1;
            if !self.unbonding_amount.read(&idx)?.is_zero() {
                return Ok(true);
            }
            current_stored = self.unbonding_next.read(&idx)?;
        }
        Ok(false)
    }

    fn finalize_inactive_if_complete(
        &self,
        val_set: &mut ValidatorSet,
        validator: Address,
    ) -> Result<()> {
        if matches!(
            val_set.validator_lifecycle(validator)?,
            ValidatorLifecycle::Unbonding(_)
        ) && self.stake_amount.read(&validator)?.is_zero()
            && !self.has_pending_unbonding(validator)?
        {
            val_set.complete_unbonding(validator)?;
        }
        Ok(())
    }

    /// Claims matured unbonding entries for the caller.
    ///
    /// Walks the per-validator linked list (O(k) where k = caller's entries),
    /// zeroes out mature entries, rebuilds the list without them,
    /// and transfers the total claimable amount to the caller.
    pub fn claim_unbonded(&mut self, caller: Address) -> Result<()> {
        let timestamp = self.storage.timestamp()?.to::<u64>();
        let mut total_claimable = U256::ZERO;

        // Walk per-validator linked list (stored = idx + 1, 0 = empty/end)
        let mut current_stored = self.per_val_unbonding_head.read(&caller)?;
        let mut new_head_stored: u32 = 0;
        let mut pending_tail_stored: u32 = 0;

        while current_stored != 0 {
            let idx = current_stored - 1;
            let next_stored = self.unbonding_next.read(&idx)?;
            let complete_time = self.unbonding_complete_time.read(&idx)?;

            if timestamp >= complete_time {
                // Mature: claim it
                let amount = self.unbonding_amount.read(&idx)?;
                total_claimable += amount;
                // Zero the entry (for tail-trim compaction by process_unbonding)
                self.unbonding_validator.write(&idx, Address::ZERO)?;
                self.unbonding_amount.write(&idx, U256::ZERO)?;
                self.unbonding_complete_time.write(&idx, 0)?;
                self.unbonding_next.write(&idx, 0)?;
            } else {
                // Not mature: keep in list
                if new_head_stored == 0 {
                    new_head_stored = current_stored;
                } else {
                    // Link previous pending entry to this one
                    self.unbonding_next
                        .write(&(pending_tail_stored - 1), current_stored)?;
                }
                pending_tail_stored = current_stored;
            }
            current_stored = next_stored;
        }

        // Terminate the rebuilt list
        if pending_tail_stored != 0 {
            self.unbonding_next.write(&(pending_tail_stored - 1), 0)?;
        }
        self.per_val_unbonding_head
            .write(&caller, new_head_stored)?;

        // Transfer accumulated claimable amount from staking contract to caller
        if !total_claimable.is_zero() {
            self.storage
                .transfer_balance(STAKING_ADDRESS, caller, total_claimable)?;
        }

        let mut val_set = ValidatorSet::new(self.storage.clone());
        self.finalize_inactive_if_complete(&mut val_set, caller)?;

        Ok(())
    }

    /// Maximum compaction operations per `process_unbonding` call.
    /// Prevents unbounded gas cost if the queue grows large.
    pub const MAX_COMPACTION_PER_BLOCK: u32 = 64;

    /// Processes validator lifecycle transitions and trims zeroed tail entries.
    ///
    /// Called each block in pre-execution. Does NOT zero mature entries.
    /// [`claim_unbonded`] zeroes them when the validator claims their funds.
    /// This function only trims zeroed tail entries to reclaim queue space.
    ///
    /// Uses tail-trim instead of swap-remove to preserve stable indices for
    /// the per-validator linked list.
    ///
    /// Capped at [`MAX_COMPACTION_PER_BLOCK`] operations per call to bound
    /// per-block cost. Subsequent blocks trim the remaining entries.
    pub fn process_unbonding(&mut self, timestamp: u64) -> Result<()> {
        let mut val_set = ValidatorSet::new(self.storage.clone());
        let validators = val_set.registered_validator_addresses()?;
        for validator in validators {
            let state = val_set.validator_state(validator)?;
            if matches!(state.lifecycle(), ValidatorLifecycle::Unbonding(_)) {
                self.retire_unbonding_validator(&mut val_set, validator, &state, timestamp)?;
            }
        }
        self.trim_unbonding_tail()
    }

    /// Moves the bonded stake of an `Unbonding` validator into the unbonding
    /// queue. A validator without bonded stake completes its unbonding when
    /// no queue entry remains.
    fn retire_unbonding_validator(
        &mut self,
        val_set: &mut ValidatorSet,
        validator: Address,
        state: &ValidatorState,
        timestamp: u64,
    ) -> Result<()> {
        let stake = self.stake_amount.read(&validator)?;
        if stake.is_zero() {
            return self.finalize_inactive_if_complete(val_set, validator);
        }
        let total = self.total_staked.read()?;
        if stake > total {
            return Err(PrecompileError::Revert(format!(
                "stake accounting underflow for validator {}",
                validator
            )));
        }
        self.stake_amount.write(&validator, U256::ZERO)?;
        self.total_staked.write(total - stake)?;

        let slash_count = state
            .history()
            .ok_or_else(|| {
                PrecompileError::Fatal("registered validator is missing history".into())
            })?
            .slash_count();
        let period = if slash_count > 0 {
            self.slashed_withdrawal_delay()?
        } else {
            self.config_unbonding_period.read()?
        };
        let complete_time = self.checked_complete_time(timestamp, period)?;
        self.enqueue_unbonding(validator, stake, complete_time)?;
        val_set.record_unstake(validator, U256::ZERO, U256::ZERO, complete_time)
    }

    /// Trims zeroed entries from the queue tail only, which keeps the indices
    /// of the per-validator linked lists stable. At most
    /// [`Self::MAX_COMPACTION_PER_BLOCK`] entries are trimmed per call.
    fn trim_unbonding_tail(&mut self) -> Result<()> {
        let mut count = self.unbonding_count.read()?;
        let mut ops: u32 = 0;

        while count > 0 && ops < Self::MAX_COMPACTION_PER_BLOCK {
            let validator = self.unbonding_validator.read(&(count - 1))?;
            if !validator.is_zero() {
                break;
            }
            count -= 1;
            ops += 1;
        }

        self.unbonding_count.write(count)
    }
}
