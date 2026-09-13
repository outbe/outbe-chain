use super::status;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, StakeProjection, ValidatorLifecycle};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

impl ValidatorSet<'_> {
    /// Records a successful stake increase from Staking and performs the
    /// REGISTERED -> PENDING threshold transition when required.
    ///
    /// Staking owns the authoritative balance. This method is the only
    /// production write seam for the ValidatorSet mirror and its coupled
    /// lifecycle fields.
    pub fn record_stake_increase(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let stake = StakeProjection::new(bonded, before.unbonding_end_hint());
        let (lifecycle, became_pending) = match before.lifecycle().clone() {
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert(
                    "cannot stake before validator registration".into(),
                ));
            }
            ValidatorLifecycle::Inactive(_) => {
                return Err(PrecompileError::Revert(
                    "inactive validator must re-register before staking".into(),
                ));
            }
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

        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        if became_pending {
            self.pending_set_change.write(true)?;
        }
        guard.commit();
        if became_pending {
            crate::metrics::record_validator_status(addr, status::PENDING);
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Records a voluntary withdrawal and applies the complete coupled lifecycle
    /// transition. Readiness is consumed on demotion, and a jailed validator may
    /// leave only after fully unstaking.
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
                    joining, stake, minimum,
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

        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        if set_change {
            self.pending_set_change.write(true)?;
        }
        guard.commit();
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
                    joining, stake, minimum,
                )?)
            }
            ValidatorLifecycle::Active(active) if below_minimum => {
                set_change = true;
                ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, height)?)
            }
            lifecycle => state_machine::with_stake(lifecycle, stake)?,
        };
        let after = before.clone().with_lifecycle(next)?;

        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        if set_change {
            self.pending_set_change.write(true)?;
        }
        guard.commit();
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
        let cleared = match state_machine::with_stake(
            ValidatorLifecycle::Unbonding(unbonding),
            StakeProjection::new(before.bonded_stake(), None),
        )? {
            ValidatorLifecycle::Unbonding(unbonding) => unbonding,
            _ => unreachable!("with_stake preserves lifecycle variant"),
        };
        let inactive = state_machine::complete_unbonding(cleared)?;
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::Inactive(inactive))?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        guard.commit();
        Ok(())
    }
}
