use crate::errors::ValidatorSetError;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, StakeProjection, ValidatorLifecycle};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

impl ValidatorSet<'_> {
    /// Records a successful stake increase (`topup`) from Staking.
    ///
    /// Staking owns the authoritative balance and passes the post-write amount.
    /// This method, `record_unstake`, `record_stake_slash`, and
    /// `complete_unbonding` update lifecycle columns only.
    pub fn record_stake_increase(
        &mut self,
        addr: Address,
        bonded: U256,
        _minimum: U256,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let stake = StakeProjection::new(bonded, before.unbonding_end_hint());
        if let Some(rejection) = state_machine::stake_target_rejection(before.lifecycle()) {
            return Err(rejection);
        }
        let lifecycle = match before.lifecycle().clone() {
            ValidatorLifecycle::Exiting(_) | ValidatorLifecycle::Unbonding(_) => {
                return Err(ValidatorSetError::CannotIncreaseStakeWhileExitingOrUnbonding.into());
            }
            lifecycle => state_machine::with_stake(lifecycle, stake)?,
        };
        let after = before.clone().with_lifecycle(lifecycle)?;
        self.commit_transition(&before, &after, false)?;
        Ok(())
    }

    /// Records a voluntary withdrawal and applies the complete coupled lifecycle
    /// transition. A jailed validator may leave only after it fully unstakes.
    pub fn record_unstake(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
        unbonding_end_hint: u64,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let lifecycle = before.lifecycle().clone();
        let stake = StakeProjection::new(
            bonded,
            (unbonding_end_hint != 0).then_some(unbonding_end_hint),
        );
        let mut set_change = false;
        let height = self.storage.block_number()?;
        let below_minimum = !minimum.is_zero() && !bonded.is_zero() && bonded < minimum;
        let next = match lifecycle {
            ValidatorLifecycle::Absent => {
                return Err(ValidatorSetError::ValidatorNotRegistered.into());
            }
            ValidatorLifecycle::Inactive(_) => {
                return Err(ValidatorSetError::ValidatorIsInactive.into());
            }
            ValidatorLifecycle::JailRetained(jailed) => {
                if !bonded.is_zero() {
                    return Err(ValidatorSetError::JailedValidatorMustFullyUnstakeForExit.into());
                }
                set_change = true;
                ValidatorLifecycle::Exiting(state_machine::full_exit_jailed_retained(
                    jailed, stake,
                )?)
            }
            ValidatorLifecycle::Jail(jailed) => {
                if !bonded.is_zero() {
                    return Err(ValidatorSetError::JailedValidatorMustFullyUnstakeForExit.into());
                }
                ValidatorLifecycle::Unbonding(state_machine::full_exit_jailed(jailed, stake)?)
            }
            _ if below_minimum => {
                return Err(ValidatorSetError::BondedStakeBelowMinimum { bonded, minimum }.into());
            }
            ValidatorLifecycle::WaitingForReadiness(waiting) if bonded.is_zero() => {
                set_change = true;
                ValidatorLifecycle::Unbonding(state_machine::unbond_waiting_for_readiness(
                    waiting, stake, height,
                )?)
            }
            ValidatorLifecycle::Joining(joining) if bonded.is_zero() => {
                set_change = true;
                ValidatorLifecycle::Unbonding(state_machine::unbond_joining(
                    joining, stake, height,
                )?)
            }
            ValidatorLifecycle::Active(active) if bonded.is_zero() => {
                set_change = true;
                ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, height)?)
            }
            lifecycle => state_machine::with_stake(lifecycle, stake)?,
        };
        let after = before.clone().with_lifecycle(next)?;

        self.commit_transition(&before, &after, set_change)?;
        if set_change {
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Records a stake slash. This is intentionally distinct from voluntary
    /// withdrawal: a JAILED validator remains JAILED after the slash, and an
    /// unconfirmed `WaitingForReadiness` validator stays in `WaitingForReadiness`.
    pub fn record_stake_slash(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
        unbonding_end_hint: Option<u64>,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let lifecycle = before.lifecycle().clone();
        let hint = unbonding_end_hint.or(before.unbonding_end_hint());
        let stake = StakeProjection::new(bonded, hint);
        let mut set_change = false;
        let below_minimum = !minimum.is_zero() && bonded < minimum;
        let height = self.storage.block_number()?;
        let next = match lifecycle {
            ValidatorLifecycle::Absent => {
                return Err(ValidatorSetError::ValidatorNotRegistered.into());
            }
            ValidatorLifecycle::Inactive(_) => {
                return Err(ValidatorSetError::ValidatorIsInactive.into());
            }
            ValidatorLifecycle::Joining(joining) if below_minimum => {
                set_change = true;
                ValidatorLifecycle::WaitingForReadiness(state_machine::demote_joining(
                    joining, stake, minimum, height,
                )?)
            }
            ValidatorLifecycle::Active(active) if below_minimum => {
                set_change = true;
                ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, height)?)
            }
            lifecycle => state_machine::with_stake(lifecycle, stake)?,
        };
        let after = before.clone().with_lifecycle(next)?;

        self.commit_transition(&before, &after, set_change)?;
        if set_change {
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Completes UNBONDING after Staking has verified zero bonded stake and no
    /// remaining live claims.
    pub fn complete_unbonding(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        let unbonding = match before.lifecycle().clone() {
            ValidatorLifecycle::Unbonding(unbonding) => unbonding,
            _ => return Ok(()),
        };
        let cleared = state_machine::with_unbonding_stake(
            unbonding,
            StakeProjection::new(before.bonded_stake(), None),
        )?;
        let inactive = state_machine::complete_unbonding(cleared)?;
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::Inactive(inactive))?;
        self.commit_transition(&before, &after, false)
    }
}
