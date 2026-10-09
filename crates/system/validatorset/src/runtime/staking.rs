use super::status;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, StakeProjection, ValidatorLifecycle};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

impl ValidatorSet<'_> {
    /// Records a successful stake increase from Staking and performs the
    /// REGISTERED -> PENDING threshold transition when required.
    ///
    /// Staking owns the authoritative balance.
    /// `record_unstake`, `record_stake_slash`, `record_ocomp_bonded_slash`,
    /// and `complete_unbonding` also write the mirror and its lifecycle fields.
    pub fn record_stake_increase(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let stake = StakeProjection::new(bonded, before.unbonding_end_hint());
        if let Some(rejection) = state_machine::stake_target_rejection(before.lifecycle()) {
            return Err(rejection);
        }
        let (lifecycle, became_pending) = match before.lifecycle().clone() {
            ValidatorLifecycle::Exiting(_) | ValidatorLifecycle::Unbonding(_) => {
                return Err(PrecompileError::Revert(
                    "cannot increase stake while validator is exiting or unbonding".into(),
                ));
            }
            ValidatorLifecycle::WaitingForStake(waiting) if bonded >= minimum => (
                ValidatorLifecycle::WaitingForReadiness(state_machine::reach_minimum(
                    waiting, stake, minimum,
                )?),
                true,
            ),
            lifecycle => (state_machine::with_stake(lifecycle, stake)?, false),
        };
        let after = before.clone().with_lifecycle(lifecycle)?;

        self.commit_transition(&before, &after, became_pending)?;
        if became_pending {
            crate::metrics::record_validator_status(addr, status::PENDING);
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Records a voluntary withdrawal and applies the complete coupled lifecycle
    /// transition. A demotion consumes readiness. A jailed validator may leave
    /// only after it fully unstakes.
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
        let next = match lifecycle {
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            ValidatorLifecycle::Inactive(_) => {
                return Err(PrecompileError::Revert("validator is inactive".into()));
            }
            ValidatorLifecycle::WaitingForReadiness(waiting) if bonded < minimum => {
                set_change = true;
                ValidatorLifecycle::WaitingForStake(state_machine::demote_waiting_for_readiness(
                    waiting, stake, minimum,
                )?)
            }
            ValidatorLifecycle::Joining(joining) if bonded < minimum => {
                set_change = true;
                ValidatorLifecycle::WaitingForStake(state_machine::demote_joining(
                    joining, stake, minimum, height,
                )?)
            }
            ValidatorLifecycle::Active(active) if bonded < minimum => {
                set_change = true;
                ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, height)?)
            }
            ValidatorLifecycle::JailRetained(jailed) if bonded.is_zero() => {
                set_change = true;
                ValidatorLifecycle::Exiting(state_machine::full_exit_jailed_retained(
                    jailed, stake,
                )?)
            }
            ValidatorLifecycle::Jail(jailed) if bonded.is_zero() => {
                ValidatorLifecycle::Unbonding(state_machine::full_exit_jailed(jailed, stake)?)
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
    /// withdrawal: a JAILED validator remains JAILED after the slash.
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
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            ValidatorLifecycle::Inactive(_) => {
                return Err(PrecompileError::Revert("validator is inactive".into()));
            }
            ValidatorLifecycle::WaitingForReadiness(waiting) if below_minimum => {
                set_change = true;
                ValidatorLifecycle::WaitingForStake(state_machine::demote_waiting_for_readiness(
                    waiting, stake, minimum,
                )?)
            }
            ValidatorLifecycle::Joining(joining) if below_minimum => {
                set_change = true;
                ValidatorLifecycle::WaitingForStake(state_machine::demote_joining(
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
