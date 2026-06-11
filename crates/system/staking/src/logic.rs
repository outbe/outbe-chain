use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::logic::status;

use crate::contract::Staking;

impl Staking<'_> {
    /// Stakes `amount` on behalf of `validator`.
    ///
    /// - Adds amount to stake_amount[validator] and total_staked.
    /// - If the validator is registered in ValidatorSet and the new stake meets
    ///   min_stake, activates the validator (Phase 1 auto-activation).
    /// - Enforces max_stake_percent if configured.
    /// - Updates val_stake in ValidatorSet.
    pub fn stake(&mut self, caller: Address, validator: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be non-zero".into()));
        }

        // A-43: Enforce self-stake only — no third-party delegation.
        // Without full delegation accounting, a delegator's funds would be
        // locked with no protocol-level withdrawal mechanism.
        if caller != validator {
            return Err(PrecompileError::Revert(
                "third-party staking not supported: caller must be validator".into(),
            ));
        }

        // A-01: Do NOT call transfer_balance here. For payable precompile calls,
        // the EVM already transfers msg.value from caller to STAKING_ADDRESS
        // via CallValue::Transfer. A second transfer would double-charge the caller.

        // Update staking contract state
        let current = self.stake_amount.read(&validator)?;
        let new_stake = current + amount;

        // Enforce max_stake_percent if configured
        let max_pct = self.config_max_stake_percent.read()?;
        if max_pct > 0 && max_pct < 100 {
            let total = self.total_staked.read()?;
            if total.is_zero() {
                self.stake_amount.write(&validator, new_stake)?;

                self.total_staked.write(amount)?;

                let mut val_set = ValidatorSet::new(self.storage.clone());
                val_set.val_stake.write(&validator, new_stake)?;

                let min_stake = self.config_min_stake.read()?;
                if val_set.is_validator(validator)? {
                    let current_status = val_set.val_status.read(&validator)?;
                    if new_stake >= min_stake && current_status == status::REGISTERED {
                        // PoS: stake reaching min_stake marks the validator PENDING
                        // (admitted, syncing, not yet voting). The next DKG reshare
                        // grants a share and promotes PENDING→ACTIVE.
                        val_set.mark_pending(validator)?;
                    }
                }

                return Ok(());
            }
            let new_total = total + amount;
            // Check: new_stake / new_total <= max_pct / 100
            // Equivalent to: new_stake * 100 <= max_pct * new_total
            if new_stake * U256::from(100u64) > U256::from(max_pct) * new_total {
                return Err(PrecompileError::Revert(
                    "stake would exceed max_stake_percent".into(),
                ));
            }
        }

        self.stake_amount.write(&validator, new_stake)?;

        let total = self.total_staked.read()?;
        self.total_staked.write(total + amount)?;

        // Cross-call: update ValidatorSet
        let mut val_set = ValidatorSet::new(self.storage.clone());

        // Update val_stake in ValidatorSet
        val_set.val_stake.write(&validator, new_stake)?;

        // PoS staking: when a REGISTERED validator reaches min_stake it becomes
        // PENDING (admitted to the validator set, syncing, not yet voting). The next
        // DKG reshare grants it a share and activate_reshared_set promotes
        // PENDING→ACTIVE. mark_pending also raises pending_set_change so consensus
        // schedules that reshare.
        let min_stake = self.config_min_stake.read()?;
        if val_set.is_validator(validator)? {
            let current_status = val_set.val_status.read(&validator)?;
            if new_stake >= min_stake && current_status == status::REGISTERED {
                val_set.mark_pending(validator)?;
            }
        }

        Ok(())
    }

    fn checked_complete_time(&self, timestamp: u64, period: u64) -> Result<u64> {
        timestamp.checked_add(period).ok_or_else(|| {
            PrecompileError::Revert("unbonding completion timestamp overflow".into())
        })
    }

    fn slashed_withdrawal_delay(&self) -> Result<u64> {
        let configured = self.config_slashed_withdrawal_delay.read()?;
        if configured > 0 {
            return Ok(configured);
        }
        let unbonding_period = self.config_unbonding_period.read()?;
        unbonding_period
            .checked_mul(2)
            .ok_or_else(|| PrecompileError::Revert("slashed withdrawal delay overflow".into()))
    }

    fn enqueue_unbonding(
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
        if !val_set.is_validator(validator)? {
            return Ok(());
        }
        let current_status = val_set.val_status.read(&validator)?;
        if current_status == status::UNBONDING
            && self.stake_amount.read(&validator)?.is_zero()
            && !self.has_pending_unbonding(validator)?
        {
            val_set.val_status.write(&validator, status::INACTIVE)?;
            val_set.val_unbonding_end.write(&validator, 0)?;
            val_set.val_has_bls_share.write(&validator, false)?;
        }
        Ok(())
    }

    /// Unstakes `amount` from the caller (self-unstake only).
    ///
    /// - Reduces stake_amount[caller] and total_staked by amount.
    /// - If stake falls below min_stake and validator is ACTIVE, transitions
    ///   to EXITING (awaiting DKG reshare to exclude from consensus set).
    /// - Enqueues an unbonding entry with complete_time = now + unbonding_period.
    /// - Updates val_stake in ValidatorSet.
    pub fn unstake(&mut self, caller: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be non-zero".into()));
        }

        let current = self.stake_amount.read(&caller)?;
        if amount > current {
            return Err(PrecompileError::Revert("insufficient staked amount".into()));
        }

        let new_stake = current - amount;
        self.stake_amount.write(&caller, new_stake)?;

        let total = self.total_staked.read()?;
        self.total_staked.write(total - amount)?;

        // Cross-call: update ValidatorSet
        let val_set = ValidatorSet::new(self.storage.clone());
        val_set.val_stake.write(&caller, new_stake)?;

        let min_stake = self.config_min_stake.read()?;
        if val_set.is_validator(caller)? {
            let current_status = val_set.val_status.read(&caller)?;
            if new_stake < min_stake && current_status == status::ACTIVE {
                // Transition to EXITING — DKG reshare will exclude from consensus set
                val_set.val_status.write(&caller, status::EXITING)?;
                val_set
                    .val_deactivated_at_height
                    .write(&caller, val_set.storage.block_number()?)?;
                // Signal consensus to trigger DKG reshare
                val_set.pending_set_change.write(true)?;
            } else if new_stake < min_stake && current_status == status::PENDING {
                // A PENDING joiner that drops below min_stake before its activating
                // reshare reverts to REGISTERED, so the reshare target (ACTIVE∪PENDING)
                // no longer selects it and it cannot be promoted to ACTIVE without
                // re-staking. Re-signal so consensus refreshes the target.
                val_set.val_status.write(&caller, status::REGISTERED)?;
                val_set.pending_set_change.write(true)?;
            } else if new_stake < min_stake && current_status == status::JAILED {
                // A JAILED validator that unstakes below min_stake LEAVES the set:
                // it enters the EXITING → UNBONDING → INACTIVE drain (the next reshare
                // excludes it + clears its share; process_unbonding drains the stake).
                // This is the "I no longer want to be a validator" exit from jail.
                val_set.val_status.write(&caller, status::EXITING)?;
                val_set
                    .val_deactivated_at_height
                    .write(&caller, val_set.storage.block_number()?)?;
                val_set.val_jailed_at_height.write(&caller, 0)?;
                val_set.pending_set_change.write(true)?;
            }
        }

        // Add to unbonding queue
        let timestamp = self.storage.timestamp()?.to::<u64>();
        let unbonding_period = self.config_unbonding_period.read()?;
        let complete_time = self.checked_complete_time(timestamp, unbonding_period)?;
        self.enqueue_unbonding(caller, amount, complete_time)?;
        val_set.val_unbonding_end.write(&caller, complete_time)?;

        Ok(())
    }

    /// Unjails the caller's JAILED validator back to PENDING. Requires the
    /// caller's bonded stake to be ≥ min_stake (top up via `stake` first if a
    /// felony slash dropped it below). The JAILED→PENDING transition, the unjail
    /// cooldown, the readiness reset, and the reshare signal live in ValidatorSet
    /// (`unjail_to_pending`); afterwards the validator re-confirms readiness and is
    /// promoted PENDING→ACTIVE by the next DKG reshare. Self-only: `caller` is the
    /// validator (the precompile passes the tx sender).
    pub fn unjail_validator(&mut self, caller: Address) -> Result<()> {
        let stake = self.stake_amount.read(&caller)?;
        let min_stake = self.config_min_stake.read()?;
        if stake < min_stake {
            return Err(PrecompileError::Revert(format!(
                "unjailValidator requires stake >= min_stake: have {stake}, need {min_stake}"
            )));
        }
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.unjail_to_pending(caller)
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
                // Mature — claim it
                let amount = self.unbonding_amount.read(&idx)?;
                total_claimable += amount;
                // Zero out entry (for tail-trim compaction by process_unbonding)
                self.unbonding_validator.write(&idx, Address::ZERO)?;
                self.unbonding_amount.write(&idx, U256::ZERO)?;
                self.unbonding_complete_time.write(&idx, 0)?;
                self.unbonding_next.write(&idx, 0)?;
            } else {
                // Not mature — keep in list
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

    /// Slashes a validator by `percent` of their staked amount and unbonding entries.
    ///
    /// - Reduces stake_amount[validator] and total_staked by the slash amount.
    /// - A-04: Also proportionally reduces pending unbonding entries.
    /// - A-05: Burns slashed tokens from STAKING_ADDRESS native balance.
    /// - Updates val_stake in ValidatorSet.
    /// - Returns the total slashed amount (for evidence reward calculation).
    /// - Does NOT change validator status — severe faults are handled by
    ///   `SlashIndicator::slash_proposer()` via `force_exit_validator()`.
    pub fn slash_stake(&mut self, validator: Address, percent: u64) -> Result<U256> {
        if percent > 100 {
            return Err(PrecompileError::Revert(
                "slash percent must be <= 100".into(),
            ));
        }

        let current = self.stake_amount.read(&validator)?;
        let mut total_slashed = U256::ZERO;

        // Slash active stake
        if !current.is_zero() {
            let slash = current * U256::from(percent) / U256::from(100u64);
            let new_stake = current - slash;
            self.stake_amount.write(&validator, new_stake)?;
            let total = self.total_staked.read()?;
            self.total_staked.write(total - slash)?;
            total_slashed += slash;
        }

        // A-04: Slash unbonding entries proportionally.
        // Walk the per-validator linked list and reduce each pending entry.
        let mut current_stored = self.per_val_unbonding_head.read(&validator)?;
        let slash_complete_time = self.checked_complete_time(
            self.storage.timestamp()?.to::<u64>(),
            self.slashed_withdrawal_delay()?,
        )?;
        while current_stored != 0 {
            let idx = current_stored - 1;
            let amount = self.unbonding_amount.read(&idx)?;
            if !amount.is_zero() {
                let unbonding_slash = amount * U256::from(percent) / U256::from(100u64);
                if !unbonding_slash.is_zero() {
                    self.unbonding_amount
                        .write(&idx, amount - unbonding_slash)?;
                    total_slashed += unbonding_slash;
                }
                let complete_time = self.unbonding_complete_time.read(&idx)?;
                if complete_time < slash_complete_time {
                    self.unbonding_complete_time
                        .write(&idx, slash_complete_time)?;
                }
            }
            current_stored = self.unbonding_next.read(&idx)?;
        }

        // A-05: Burn slashed tokens from STAKING_ADDRESS so native balance stays
        // in sync with accounting. Without this, slashed amounts become orphaned.
        if !total_slashed.is_zero() {
            self.storage
                .decrease_balance(STAKING_ADDRESS, total_slashed)?;
        }

        // Cross-call: update ValidatorSet stake
        let remaining_stake = self.stake_amount.read(&validator)?;
        let val_set = ValidatorSet::new(self.storage.clone());
        val_set.val_stake.write(&validator, remaining_stake)?;

        // If stake dropped below min_stake, demote: an ACTIVE validator exits
        // (ACTIVE→EXITING, removed at the next reshare); a PENDING joiner that never
        // activated reverts to REGISTERED so the reshare target no longer selects it.
        let min_stake = self.config_min_stake.read()?;
        if !min_stake.is_zero() && remaining_stake < min_stake && val_set.is_validator(validator)? {
            let current_status = val_set.val_status.read(&validator)?;
            if current_status == status::ACTIVE {
                val_set.val_status.write(&validator, status::EXITING)?;
                val_set
                    .val_deactivated_at_height
                    .write(&validator, val_set.storage.block_number()?)?;
                val_set.pending_set_change.write(true)?;
            } else if current_status == status::PENDING {
                val_set.val_status.write(&validator, status::REGISTERED)?;
                val_set.pending_set_change.write(true)?;
            }
        }

        Ok(total_slashed)
    }

    /// Maximum compaction operations per `process_unbonding` call.
    /// Prevents unbounded gas cost if the queue grows large.
    pub const MAX_COMPACTION_PER_BLOCK: u32 = 64;

    /// Processes validator lifecycle transitions and trims zeroed tail entries.
    ///
    /// Called each block in pre-execution. Does NOT zero out mature entries —
    /// that is done by [`claim_unbonded`] when the validator claims their funds.
    /// This function only trims zeroed tail entries to reclaim queue space.
    ///
    /// Uses tail-trim instead of swap-remove to preserve stable indices for
    /// the per-validator linked list.
    ///
    /// Capped at [`MAX_COMPACTION_PER_BLOCK`] operations per call to bound
    /// per-block cost. Remaining entries are trimmed in subsequent blocks.
    pub fn process_unbonding(&mut self, timestamp: u64) -> Result<()> {
        let mut val_set = ValidatorSet::new(self.storage.clone());
        let validators = val_set.get_all_validators()?;
        for v in validators {
            if v.status != status::UNBONDING {
                continue;
            }

            let stake = self.stake_amount.read(&v.validator_address)?;
            if !stake.is_zero() {
                let total = self.total_staked.read()?;
                if stake > total {
                    return Err(PrecompileError::Revert(format!(
                        "stake accounting underflow for validator {}",
                        v.validator_address
                    )));
                }
                self.stake_amount.write(&v.validator_address, U256::ZERO)?;
                self.total_staked.write(total - stake)?;
                val_set.val_stake.write(&v.validator_address, U256::ZERO)?;

                let period = if v.slash_count > 0 {
                    self.slashed_withdrawal_delay()?
                } else {
                    self.config_unbonding_period.read()?
                };
                let complete_time = self.checked_complete_time(timestamp, period)?;
                self.enqueue_unbonding(v.validator_address, stake, complete_time)?;
                val_set
                    .val_unbonding_end
                    .write(&v.validator_address, complete_time)?;
            } else {
                self.finalize_inactive_if_complete(&mut val_set, v.validator_address)?;
            }
        }

        let mut count = self.unbonding_count.read()?;
        let mut ops: u32 = 0;

        // Trim zeroed entries from tail only (preserves linked list indices)
        while count > 0 && ops < Self::MAX_COMPACTION_PER_BLOCK {
            let validator = self.unbonding_validator.read(&(count - 1))?;
            if !validator.is_zero() {
                break;
            }
            count -= 1;
            ops += 1;
        }

        self.unbonding_count.write(count)?;

        Ok(())
    }

    /// Returns the staked amount for a validator.
    pub fn get_stake(&self, validator: Address) -> Result<U256> {
        self.stake_amount.read(&validator)
    }

    /// Returns the total staked amount across all validators.
    pub fn get_total_staked(&self) -> Result<U256> {
        self.total_staked.read()
    }
}
