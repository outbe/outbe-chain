use alloy_primitives::{Address, B256, U256, U512};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::Result;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::errors::ValidatorSetError;
use outbe_validatorset::ValidatorLifecycle;

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

    /// Atomically registers `validator` (or re-registers an `INACTIVE`
    /// validator) and bonds `amount >= min_stake`, transitioning directly to
    /// `WaitingForReadiness` (`PENDING`).
    pub fn stake(
        &mut self,
        caller: Address,
        validator: Address,
        amount: U256,
        (consensus_pubkey, radicle_node_id, bls_signature): (&[u8; 48], B256, &[u8; 96]),
        (p2p_version, p2p_encoded): (u8, &[u8]),
    ) -> Result<()> {
        if amount.is_zero() {
            return Err(StakingError::AmountZero.into());
        }
        if caller != validator {
            return Err(StakingError::ThirdPartyStakingNotSupported.into());
        }
        let min_stake = self.config_min_stake.read()?;
        if amount < min_stake {
            return Err(ValidatorSetError::BondedStakeBelowMinimum {
                bonded: amount,
                minimum: min_stake,
            }
            .into());
        }

        let (new_stake, new_total) = self.checked_stake_credit(validator, amount)?;

        // Register in ValidatorSet before writing `stake_amount` so that
        // `ValidatorSet::validator_state(validator)` decodes `Absent` or
        // `Inactive` with zero prior bonded stake.
        let guard = self.storage.checkpoint_guard();
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.register_with_initial_stake(
            caller,
            validator,
            (consensus_pubkey, radicle_node_id, Some(bls_signature)),
            (p2p_version, p2p_encoded),
            (new_stake, min_stake),
        )?;
        self.stake_amount.write(&validator, new_stake)?;
        self.total_staked.write(new_total)?;
        self.emit_staking_event(IStaking::Staked {
            validator,
            amount,
            bonded: new_stake,
        })?;
        guard.commit();

        Ok(())
    }

    /// Adds `amount` to an already-registered validator's bonded stake.
    pub fn topup(&mut self, caller: Address, validator: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(StakingError::AmountZero.into());
        }
        if caller != validator {
            return Err(StakingError::ThirdPartyStakingNotSupported.into());
        }

        let (new_stake, new_total) = self.checked_stake_credit(validator, amount)?;
        let min_stake = self.config_min_stake.read()?;

        let guard = self.storage.checkpoint_guard();
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_stake_increase(validator, new_stake, min_stake)?;
        self.stake_amount.write(&validator, new_stake)?;
        self.total_staked.write(new_total)?;
        self.emit_staking_event(IStaking::Staked {
            validator,
            amount,
            bonded: new_stake,
        })?;
        guard.commit();

        Ok(())
    }

    fn checked_stake_credit(&self, validator: Address, amount: U256) -> Result<(U256, U256)> {
        let open_entries = self.per_val_unbonding_len.read(&validator)?;
        let max_partial = crate::contract::MAX_UNBONDING_ENTRIES_PER_VALIDATOR - 1;
        if open_entries >= max_partial {
            return Err(StakingError::TooManyUnbondingEntries {
                validator,
                max_entries: max_partial,
            }
            .into());
        }

        let current = self.stake_amount.read(&validator)?;
        let new_stake = current
            .checked_add(amount)
            .ok_or(StakingError::StakeAccountingOverflow { validator })?;
        let total = self.total_staked.read()?;
        let new_total = total
            .checked_add(amount)
            .ok_or(StakingError::TotalStakeAccountingOverflow)?;

        let max_pct = self.config_max_stake_percent.read()?;
        // When `total` is zero, the first staker holds 100% of `new_total`.
        // Skip the concentration check on an empty pool so the first validator can bond.
        if max_pct > 0
            && max_pct < 100
            && !total.is_zero()
            && U512::from(new_stake) * U512::from(100u64)
                > U512::from(max_pct) * U512::from(new_total)
        {
            return Err(StakingError::StakeExceedsMaxStakePercent.into());
        }
        Ok((new_stake, new_total))
    }

    fn ensure_zero_unstake_allowed(&self, caller: Address, current: U256) -> Result<()> {
        if !current.is_zero() {
            return Err(StakingError::AmountZero.into());
        }
        let val_set = ValidatorSet::new(self.storage.clone());
        match val_set.validator_lifecycle(caller)? {
            ValidatorLifecycle::Absent => Err(ValidatorSetError::ValidatorNotRegistered.into()),
            ValidatorLifecycle::Inactive(_) => Err(ValidatorSetError::ValidatorIsInactive.into()),
            ValidatorLifecycle::WaitingForReadiness(_)
            | ValidatorLifecycle::Joining(_)
            | ValidatorLifecycle::JailRetained(_)
            | ValidatorLifecycle::Jail(_) => Ok(()),
            ValidatorLifecycle::Active(_)
            | ValidatorLifecycle::Exiting(_)
            | ValidatorLifecycle::Unbonding(_) => Err(StakingError::AmountZero.into()),
        }
    }

    fn ensure_unbonding_capacity(&self, validator: Address, full_exit: bool) -> Result<()> {
        let open_entries = self.per_val_unbonding_len.read(&validator)?;
        let max_entries = if full_exit {
            crate::contract::MAX_UNBONDING_ENTRIES_PER_VALIDATOR
        } else {
            crate::contract::MAX_UNBONDING_ENTRIES_PER_VALIDATOR - 1
        };
        if open_entries >= max_entries {
            return Err(StakingError::TooManyUnbondingEntries {
                validator,
                max_entries,
            }
            .into());
        }
        Ok(())
    }

    /// Unstakes `amount` from the caller (self-unstake only).
    ///
    /// - Reduces stake_amount[caller] and total_staked by amount.
    /// - Requires the remaining bonded stake to be either `0` or `>= min_stake`
    ///   (and requires full unstake for jailed validators).
    /// - Allows `unstake(0)` when `current == 0` in `WaitingForReadiness`,
    ///   `Joining`, `JailRetained`, or `Jail` so a 100%-slashed validator can
    ///   voluntarily exit without creating a zero-amount queue entry.
    pub fn unstake(&mut self, caller: Address, amount: U256) -> Result<()> {
        let current = self.stake_amount.read(&caller)?;
        if amount.is_zero() {
            self.ensure_zero_unstake_allowed(caller, current)?;
        }
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

        if !amount.is_zero() {
            self.ensure_unbonding_capacity(caller, new_stake.is_zero())?;
        }

        let guard = self.storage.checkpoint_guard();
        if !amount.is_zero() {
            self.stake_amount.write(&caller, new_stake)?;
            self.total_staked.write(new_total)?;
        }

        let started = self.enqueue_unbonding(caller, amount, complete_time)?;
        let hint = if started.is_some() || self.has_pending_unbonding(caller)? {
            complete_time
        } else {
            0
        };
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_unstake(caller, new_stake, min_stake, hint)?;
        self.finalize_inactive_if_complete(&mut val_set, caller)?;
        if let Some(index) = started {
            self.emit_staking_event(IStaking::UnbondingStarted {
                validator: caller,
                index,
                amount,
                completeTime: complete_time,
                automatic: false,
            })?;
        }
        guard.commit();

        Ok(())
    }

    /// Unjails the caller's JAILED validator back to PENDING. Requires the
    /// caller's bonded stake to be >= min_stake. If a felony slash dropped it
    /// below, top up via `topup` first. ValidatorSet (`unjail_after_stake_check`)
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
    ///   `Joining` moves to `WaitingForReadiness`. `Active` moves to `Exiting`.
    ///   SlashIndicator felony paths jail the validator. They do not force-exit it.
    pub fn slash_stake(&mut self, validator: Address, percent: u64) -> Result<U256> {
        if percent > 100 {
            return Err(StakingError::InvalidSlashPercent.into());
        }

        let guard = self.storage.checkpoint_guard();
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

        // Burn slashed units from STAKING_ADDRESS and credit the Promis Limit.
        if !total_slashed.is_zero() {
            outbe_promislimit::PromisLimitContract::new(self.storage.clone())
                .burn_native_to_promis_limit(
                    STAKING_ADDRESS,
                    total_slashed,
                    outbe_promislimit::NativeBurnSource::Slash,
                )?;
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
        guard.commit();

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
