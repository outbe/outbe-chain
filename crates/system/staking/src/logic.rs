use alloy_primitives::{Address, U256, U512};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::Result;
use outbe_validatorset::contract::ValidatorSet;

use crate::contract::Staking;
use crate::errors::StakingError;
use crate::precompile::IStaking;

fn slash_percent(amount: U256, percent: u64) -> Result<U256> {
    let hundred = U256::from(100u64);
    let percent = U256::from(percent);
    // For percent <= 100, the first term and the sum cannot exceed amount.
    // The remainder product cannot exceed 9,900.
    let quotient = (amount / hundred)
        .checked_mul(percent)
        .ok_or(StakingError::SlashAmountOverflow)?;
    let remainder = (amount % hundred)
        .checked_mul(percent)
        .ok_or(StakingError::SlashAmountOverflow)?
        / hundred;
    quotient
        .checked_add(remainder)
        .ok_or_else(|| StakingError::SlashAmountOverflow.into())
}

impl Staking<'_> {
    pub(crate) fn emit_staking_event<E: SolEvent>(&self, event: E) -> Result<()> {
        self.storage
            .emit_event(STAKING_ADDRESS, event.encode_log_data())
    }

    /// Stakes `amount` on behalf of `validator`.
    ///
    /// - Adds amount to stake_amount[validator] and total_staked.
    /// - If the validator is registered and the new stake meets min_stake,
    ///   moves it to PENDING (`WaitingForReadiness`).
    ///   ACTIVE comes later, from reshare activation.
    /// - Enforces max_stake_percent if configured.
    /// - Passes the post-write amount to ValidatorSet for the lifecycle transition.
    pub fn stake(&mut self, caller: Address, validator: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(StakingError::AmountZero.into());
        }

        // Enforce self-stake only. Third-party delegation is not allowed.
        // Without full delegation accounting, a delegator's funds would be
        // locked with no protocol-level withdrawal mechanism.
        if caller != validator {
            return Err(StakingError::ThirdPartyStakingNotSupported.into());
        }

        // Do NOT call transfer_balance here. For payable precompile calls,
        // the EVM already transfers msg.value from caller to STAKING_ADDRESS
        // via CallValue::Transfer. A second transfer would double-charge the caller.

        // Update staking contract state
        let current = self.stake_amount.read(&validator)?;
        let new_stake = current
            .checked_add(amount)
            .ok_or(StakingError::StakeAccountingOverflow { validator })?;
        let total = self.total_staked.read()?;
        let new_total = total
            .checked_add(amount)
            .ok_or(StakingError::TotalStakeAccountingOverflow)?;

        // Enforce max_stake_percent if configured
        let max_pct = self.config_max_stake_percent.read()?;
        if max_pct > 0 && max_pct < 100 && !total.is_zero() {
            // Check: new_stake / new_total <= max_pct / 100
            // Equivalent to: new_stake * 100 <= max_pct * new_total
            // Each product of a U256 value and a percentage <= 100 fits in U512.
            // The comparison does not narrow either product.
            if U512::from(new_stake) * U512::from(100u64)
                > U512::from(max_pct) * U512::from(new_total)
            {
                return Err(StakingError::StakeExceedsMaxStakePercent.into());
            }
        }

        self.stake_amount.write(&validator, new_stake)?;

        self.total_staked.write(new_total)?;

        // PoS staking: when a REGISTERED validator reaches min_stake it becomes
        // PENDING (`WaitingForReadiness`). It is admitted, not yet voting.
        // A later reshare activation promotes PENDING to ACTIVE.
        // The ValidatorSet facade mirrors the bonded value and raises
        // pending_set_change when the stake crosses the threshold.
        let min_stake = self.config_min_stake.read()?;
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_stake_increase(validator, new_stake, min_stake)?;
        self.emit_staking_event(IStaking::Staked {
            validator,
            amount,
            bonded: new_stake,
        })?;

        Ok(())
    }

    /// Unstakes `amount` from the caller (self-unstake only).
    ///
    /// - Reduces stake_amount[caller] and total_staked by amount.
    /// - If stake falls below min_stake and validator is ACTIVE, transitions
    ///   to EXITING (awaiting DKG reshare to exclude from consensus set).
    /// - Enqueues an unbonding entry with complete_time = now + unbonding_period.
    /// - Passes the post-write amount to ValidatorSet for the lifecycle transition.
    pub fn unstake(&mut self, caller: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(StakingError::AmountZero.into());
        }

        let current = self.stake_amount.read(&caller)?;
        if amount > current {
            return Err(StakingError::InsufficientStakedAmount.into());
        }

        let timestamp = self.storage.now_secs()?;
        let unbonding_period = self.config_unbonding_period.read()?;
        let complete_time = self.checked_complete_time(timestamp, unbonding_period)?;
        let min_stake = self.config_min_stake.read()?;
        let new_stake = current
            .checked_sub(amount)
            .ok_or(StakingError::StakeAccountingUnderflow { validator: caller })?;
        let total = self.total_staked.read()?;
        let new_total = total
            .checked_sub(amount)
            .ok_or(StakingError::StakeAccountingUnderflow { validator: caller })?;
        self.stake_amount.write(&caller, new_stake)?;
        self.total_staked.write(new_total)?;

        // Staking owns the accounting and queue. The ValidatorSet facade records
        // the complete projection only after those authoritative writes succeed.
        // The outer call-frame checkpoint keeps the sequence atomic on failure.
        let started = self.enqueue_unbonding(caller, amount, complete_time)?;
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_unstake(caller, new_stake, min_stake, complete_time)?;
        if let Some(index) = started {
            self.emit_staking_event(IStaking::UnbondingStarted {
                validator: caller,
                index,
                amount,
                completeTime: complete_time,
                automatic: false,
            })?;
        }

        Ok(())
    }

    /// Unjails the caller's JAILED validator back to PENDING. Requires the
    /// caller's bonded stake to be >= min_stake. If a felony slash dropped it
    /// below, top up via `stake` first. ValidatorSet (`unjail_after_stake_check`)
    /// holds these steps:
    /// - the JAILED->PENDING transition
    /// - the unjail cooldown
    /// - the readiness reset
    /// - the reshare signal
    ///
    /// After that, the validator re-confirms readiness, and the next DKG reshare
    /// promotes it PENDING->ACTIVE. Self-only: `caller` is the validator (the
    /// precompile passes the tx sender).
    pub fn unjail_validator(&mut self, caller: Address) -> Result<()> {
        let registry = outbe_teeregistry::TeeRegistry::new(self.storage.clone());
        if !registry.enclave_upgrade_v1()?.proposal_id.is_zero()
            && !registry.is_validator_enclave_ready_v1(caller)?
        {
            return Err(StakingError::UnjailRequiresLiveEnclaveBinding.into());
        }
        let stake = self.stake_amount.read(&caller)?;
        let min_stake = self.config_min_stake.read()?;
        if stake < min_stake {
            return Err(StakingError::UnjailInsufficientStake {
                have: stake,
                need: min_stake,
            }
            .into());
        }
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.unjail_after_stake_check(caller)
    }

    /// Slashes one unbonding row. A zero amount returns before the deadline read.
    ///
    /// The event is deferred. The caller emits it only after the native burn and
    /// the ValidatorSet record succeed. Amount uses the same floor division.
    /// The amount write stays before the deadline read. The deadline write is
    /// the max of the stored time and `slash_complete_time`. A money change or
    /// a time change returns an update, including remaining zero.
    fn slash_unbonding_entry(
        &mut self,
        validator: Address,
        index: u32,
        percent: u64,
        slash_complete_time: u64,
    ) -> Result<(U256, Option<IStaking::UnbondingUpdated>)> {
        let amount = self.unbonding_amount.read(&index)?;
        if amount.is_zero() {
            return Ok((U256::ZERO, None));
        }

        let unbonding_slash = slash_percent(amount, percent)?;
        let mut remaining = amount;
        let mut changed = false;
        if !unbonding_slash.is_zero() {
            remaining = amount
                .checked_sub(unbonding_slash)
                .ok_or(StakingError::StakeAccountingUnderflow { validator })?;
            self.unbonding_amount.write(&index, remaining)?;
            changed = true;
        }

        let complete_time = self.unbonding_complete_time.read(&index)?;
        let mut effective = complete_time;
        if complete_time < slash_complete_time {
            self.unbonding_complete_time
                .write(&index, slash_complete_time)?;
            effective = slash_complete_time;
            changed = true;
        }

        let update = if changed {
            Some(IStaking::UnbondingUpdated {
                validator,
                index,
                remaining,
                completeTime: effective,
            })
        } else {
            None
        };
        Ok((unbonding_slash, update))
    }

    /// Slashes a validator by `percent` of their staked amount and unbonding entries.
    ///
    /// - Reduces stake_amount[validator] and total_staked by the slash amount.
    /// - Also proportionally reduces pending unbonding entries.
    /// - Burns slashed tokens from STAKING_ADDRESS native balance.
    /// - Passes the post-write amount to ValidatorSet for the lifecycle transition.
    /// - Returns the total slashed amount (for evidence reward calculation).
    /// - Can change lifecycle when the remaining stake falls below the minimum.
    ///   PENDING moves to `WaitingForStake`. ACTIVE moves to EXITING.
    ///   SlashIndicator felony paths jail the validator. They do not force-exit it.
    pub fn slash_stake(&mut self, validator: Address, percent: u64) -> Result<U256> {
        if percent > 100 {
            return Err(StakingError::InvalidSlashPercent.into());
        }

        let current = self.stake_amount.read(&validator)?;
        let timestamp = self.storage.now_secs()?;
        let mut total_slashed = U256::ZERO;
        let mut bonded_slashed = U256::ZERO;

        // Slash active stake
        if !current.is_zero() {
            let slash = slash_percent(current, percent)?;
            let new_stake = current
                .checked_sub(slash)
                .ok_or(StakingError::StakeAccountingUnderflow { validator })?;
            let total = self.total_staked.read()?;
            let new_total = total
                .checked_sub(slash)
                .ok_or(StakingError::StakeAccountingUnderflow { validator })?;
            total_slashed = total_slashed
                .checked_add(slash)
                .ok_or(StakingError::SlashAmountOverflow)?;
            self.stake_amount.write(&validator, new_stake)?;
            self.total_staked.write(new_total)?;
            bonded_slashed = slash;
        }

        // Slash unbonding entries proportionally.
        // Walk the per-validator linked list and reduce each pending entry.
        let mut current_stored = self.per_val_unbonding_head.read(&validator)?;
        let slash_complete_time =
            self.checked_complete_time(timestamp, self.slashed_withdrawal_delay()?)?;
        let mut unbonding_slashed = U256::ZERO;
        let mut updates = Vec::new();
        while current_stored != 0 {
            let idx = current_stored - 1;
            let (unbonding_slash, update) =
                self.slash_unbonding_entry(validator, idx, percent, slash_complete_time)?;
            if !unbonding_slash.is_zero() {
                total_slashed = total_slashed
                    .checked_add(unbonding_slash)
                    .ok_or(StakingError::SlashAmountOverflow)?;
                unbonding_slashed = unbonding_slashed
                    .checked_add(unbonding_slash)
                    .ok_or(StakingError::SlashAmountOverflow)?;
            }
            if let Some(event) = update {
                updates.push(event);
            }
            current_stored = self.unbonding_next.read(&idx)?;
        }

        // Burn slashed tokens from STAKING_ADDRESS so native balance stays
        // in sync with accounting. Without this, slashed amounts become orphaned.
        if !total_slashed.is_zero() {
            self.storage
                .decrease_balance(STAKING_ADDRESS, total_slashed)?;
        }

        // Cross-call: pass the post-write amount after stake, claim, and burn
        // accounting succeeds. Preserve the existing unbonding-end hint.
        // Individual Staking claim timestamps remain authoritative.
        let remaining_stake = self.stake_amount.read(&validator)?;
        let min_stake = self.config_min_stake.read()?;
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_stake_slash(validator, remaining_stake, min_stake, None)?;

        for event in updates {
            self.emit_staking_event(event)?;
        }
        if !bonded_slashed.is_zero() || !unbonding_slashed.is_zero() {
            self.emit_staking_event(IStaking::Slashed {
                validator,
                bondedSlashed: bonded_slashed,
                unbondingSlashed: unbonding_slashed,
            })?;
        }

        Ok(total_slashed)
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
