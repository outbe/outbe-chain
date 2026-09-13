use super::OCOMP_RECOVERY_WINDOW_BLOCKS;
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, StakeProjection, ValidatorLifecycle};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcompMissRecord {
    Opened {
        miss_count: u64,
        recovery_deadline: u64,
    },
    Repeated {
        miss_count: u64,
        recovery_deadline: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcompRecoveryWindow {
    pub miss_count: u64,
    pub recovery_deadline: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OcompRecoveryOutcome {
    Restored = 1,
    Jailed = 2,
    NonActive = 3,
}

impl ValidatorSet<'_> {
    /// Records one missing OCOMP result vote. The first miss opens a fixed
    /// recovery window; later misses count accountability but neither extend
    /// the deadline nor authorize another bonded slash.
    pub fn record_ocomp_miss(&mut self, addr: Address) -> Result<OcompMissRecord> {
        let state = self.validator_state(addr)?;
        if !matches!(state.lifecycle(), ValidatorLifecycle::Active(_)) {
            return Err(PrecompileError::Revert(
                "OCOMP miss requires an active validator".into(),
            ));
        }

        let miss_count = self
            .val_ocomp_miss_count
            .read(&addr)?
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Fatal("OCOMP miss count overflow".into()))?;
        let existing_deadline = self.val_ocomp_recovery_deadline.read(&addr)?;
        let guard = self.storage.checkpoint_guard();
        self.val_ocomp_miss_count.write(&addr, miss_count)?;
        let outcome = if existing_deadline == 0 {
            let recovery_deadline = self
                .storage
                .block_number()?
                .checked_add(OCOMP_RECOVERY_WINDOW_BLOCKS)
                .ok_or_else(|| PrecompileError::Fatal("OCOMP recovery deadline overflow".into()))?;
            self.val_ocomp_recovery_deadline
                .write(&addr, recovery_deadline)?;
            OcompMissRecord::Opened {
                miss_count,
                recovery_deadline,
            }
        } else {
            OcompMissRecord::Repeated {
                miss_count,
                recovery_deadline: existing_deadline,
            }
        };
        guard.commit();
        Ok(outcome)
    }

    /// Mirrors the authoritative bonded stake after the one OCOMP recovery
    /// slash. This path is intentionally narrow: it requires an open recovery
    /// window and preserves ACTIVE even when the new bonded amount is below the
    /// ordinary minimum-stake threshold.
    pub fn record_ocomp_bonded_slash(&mut self, addr: Address, bonded: U256) -> Result<()> {
        if self.val_ocomp_recovery_deadline.read(&addr)? == 0 {
            return Err(PrecompileError::Revert(
                "OCOMP bonded slash requires an open recovery window".into(),
            ));
        }
        let before = self.validator_state(addr)?;
        if !matches!(before.lifecycle(), ValidatorLifecycle::Active(_)) {
            return Err(PrecompileError::Revert(
                "OCOMP bonded slash requires an active validator".into(),
            ));
        }
        let stake = StakeProjection::new(bonded, before.unbonding_end_hint());
        let lifecycle = state_machine::with_stake(before.lifecycle().clone(), stake)?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        guard.commit();
        Ok(())
    }

    /// Returns the durable OCOMP recovery state for one validator.
    pub fn ocomp_recovery_window(&self, addr: Address) -> Result<Option<OcompRecoveryWindow>> {
        let recovery_deadline = self.val_ocomp_recovery_deadline.read(&addr)?;
        if recovery_deadline == 0 {
            return Ok(None);
        }
        Ok(Some(OcompRecoveryWindow {
            miss_count: self.val_ocomp_miss_count.read(&addr)?,
            recovery_deadline,
        }))
    }

    /// Closes one recovery window while preserving cumulative accountability.
    pub fn close_ocomp_recovery_window(&mut self, addr: Address) -> Result<()> {
        self.val_ocomp_recovery_deadline.write(&addr, 0)
    }

    /// Closes one due window and emits its consensus-visible resolution.
    pub fn resolve_ocomp_recovery_window(
        &mut self,
        addr: Address,
        recovery_deadline: u64,
        bonded_stake: U256,
        outcome: OcompRecoveryOutcome,
    ) -> Result<()> {
        if self.val_ocomp_recovery_deadline.read(&addr)? != recovery_deadline {
            return Err(PrecompileError::Fatal(
                "OCOMP recovery resolution deadline mismatch".into(),
            ));
        }
        let guard = self.storage.checkpoint_guard();
        self.close_ocomp_recovery_window(addr)?;
        self.emit(IValidatorSet::OcompRecoveryResolved {
            validator: addr,
            recoveryDeadline: recovery_deadline,
            bondedStake: bonded_stake,
            outcome: outcome as u8,
        })?;
        guard.commit();
        Ok(())
    }
}
