use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_validatorset::contract::ValidatorSet;

use crate::contract::Staking;

pub use crate::ocomp_recovery::{
    OcompMissPenalty, OcompRecoveryResolution, OcompRecoverySweep, OCOMP_MISS_SLASH_PERCENT,
};

impl Staking<'_> {
    /// Stakes `amount` on behalf of `validator`.
    ///
    /// - Adds amount to stake_amount[validator] and total_staked.
    /// - If the validator is registered and the new stake meets min_stake,
    ///   moves it to PENDING (`WaitingForReadiness`).
    ///   ACTIVE comes later, from reshare activation.
    /// - Enforces max_stake_percent if configured.
    /// - Updates val_stake in ValidatorSet.
    pub fn stake(&mut self, caller: Address, validator: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(PrecompileError::Revert("amount must be non-zero".into()));
        }

        // Enforce self-stake only. Third-party delegation is not allowed.
        // Without full delegation accounting, a delegator's funds would be
        // locked with no protocol-level withdrawal mechanism.
        if caller != validator {
            return Err(PrecompileError::Revert(
                "third-party staking not supported: caller must be validator".into(),
            ));
        }

        // Do NOT call transfer_balance here. For payable precompile calls,
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

                let min_stake = self.config_min_stake.read()?;
                let mut val_set = ValidatorSet::new(self.storage.clone());
                val_set.record_stake_increase(validator, new_stake, min_stake)?;

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

        // PoS staking: when a REGISTERED validator reaches min_stake it becomes
        // PENDING (`WaitingForReadiness`). It is admitted, not yet voting.
        // A later reshare activation promotes PENDING to ACTIVE.
        // The ValidatorSet facade mirrors the bonded value and raises
        // pending_set_change when the stake crosses the threshold.
        let min_stake = self.config_min_stake.read()?;
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_stake_increase(validator, new_stake, min_stake)?;

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

        let timestamp = self.storage.timestamp()?.to::<u64>();
        let unbonding_period = self.config_unbonding_period.read()?;
        let complete_time = self.checked_complete_time(timestamp, unbonding_period)?;
        let min_stake = self.config_min_stake.read()?;
        let new_stake = current - amount;
        self.stake_amount.write(&caller, new_stake)?;

        let total = self.total_staked.read()?;
        self.total_staked.write(total - amount)?;

        // Staking owns the accounting and queue. The ValidatorSet facade records
        // the complete projection only after those authoritative writes succeed.
        // The outer call-frame checkpoint keeps the sequence atomic on failure.
        self.enqueue_unbonding(caller, amount, complete_time)?;
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_unstake(caller, new_stake, min_stake, complete_time)?;

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
            return Err(PrecompileError::Revert(
                "unjailValidator requires a live admitted enclave binding".into(),
            ));
        }
        let stake = self.stake_amount.read(&caller)?;
        let min_stake = self.config_min_stake.read()?;
        if stake < min_stake {
            return Err(PrecompileError::Revert(format!(
                "unjailValidator requires stake >= min_stake: have {stake}, need {min_stake}"
            )));
        }
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.unjail_after_stake_check(caller)
    }

    /// Slashes a validator by `percent` of their staked amount and unbonding entries.
    ///
    /// - Reduces stake_amount[validator] and total_staked by the slash amount.
    /// - Also proportionally reduces pending unbonding entries.
    /// - Burns slashed tokens from STAKING_ADDRESS native balance.
    /// - Updates val_stake in ValidatorSet.
    /// - Returns the total slashed amount (for evidence reward calculation).
    /// - Can change lifecycle when the remaining stake falls below the minimum.
    ///   PENDING moves to `WaitingForStake`. ACTIVE moves to EXITING.
    ///   SlashIndicator felony paths jail the validator. They do not force-exit it.
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

        // Slash unbonding entries proportionally.
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

        // Burn slashed tokens from STAKING_ADDRESS so native balance stays
        // in sync with accounting. Without this, slashed amounts become orphaned.
        if !total_slashed.is_zero() {
            self.storage
                .decrease_balance(STAKING_ADDRESS, total_slashed)?;
        }

        // Cross-call: mirror the authoritative stake after all stake, claim, and
        // burn accounting succeeds. Preserve the existing unbonding-end hint.
        // Individual Staking claim timestamps remain authoritative.
        let remaining_stake = self.stake_amount.read(&validator)?;
        let min_stake = self.config_min_stake.read()?;
        let mut val_set = ValidatorSet::new(self.storage.clone());
        val_set.record_stake_slash(validator, remaining_stake, min_stake, None)?;

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
