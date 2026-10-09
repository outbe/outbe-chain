//! OCOMP result-vote misses: the bonded slash of the first miss and the
//! fixed recovery window that follows it.

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::runtime::{
    DeferredValidatorPunishment, OcompMissRecord, OcompRecoveryOutcome,
};
use outbe_validatorset::ValidatorLifecycle;

use crate::contract::Staking;

pub const OCOMP_MISS_SLASH_PERCENT: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcompMissPenalty {
    pub first_in_window: bool,
    pub miss_count: u64,
    pub recovery_deadline: u64,
    pub slashed_bonded: U256,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OcompRecoverySweep {
    pub open_windows: u32,
    pub restored: u32,
    pub jailed: u32,
    pub closed_non_active: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcompRecoveryResolution {
    NotOpen,
    NotDue {
        recovery_deadline: u64,
    },
    Restored {
        recovery_deadline: u64,
    },
    Jailed {
        recovery_deadline: u64,
        observability: DeferredValidatorPunishment,
    },
    ClosedNonActive {
        recovery_deadline: u64,
    },
}

impl OcompRecoveryResolution {
    #[must_use]
    pub const fn recovery_deadline(self) -> Option<u64> {
        match self {
            Self::NotOpen => None,
            Self::NotDue { recovery_deadline }
            | Self::Restored { recovery_deadline }
            | Self::Jailed {
                recovery_deadline, ..
            }
            | Self::ClosedNonActive { recovery_deadline } => Some(recovery_deadline),
        }
    }
}

impl Staking<'_> {
    /// Records an OCOMP result-vote miss and applies the recovery policy.
    /// Only the first miss in the fixed window slashes, and that slash touches
    /// bonded stake only. The ordinary felony slash path remains unchanged.
    pub fn record_ocomp_miss(&mut self, validator: Address) -> Result<OcompMissPenalty> {
        let guard = self.storage.checkpoint_guard();
        let mut validators = ValidatorSet::new(self.storage.clone());
        let miss = validators.record_ocomp_miss(validator)?;
        let (first_in_window, miss_count, recovery_deadline) = match miss {
            OcompMissRecord::Opened {
                miss_count,
                recovery_deadline,
            } => (true, miss_count, recovery_deadline),
            OcompMissRecord::Repeated {
                miss_count,
                recovery_deadline,
            } => (false, miss_count, recovery_deadline),
        };

        let slashed_bonded = if first_in_window {
            let bonded = self.stake_amount.read(&validator)?;
            // The policy is exactly 10%, so dividing by ten is equivalent to
            // `bonded * 10 / 100` without introducing an artificial U256
            // overflow for otherwise valid bonded balances.
            let slash = bonded / U256::from(OCOMP_MISS_SLASH_PERCENT);
            let remaining = bonded - slash;
            let total = self.total_staked.read()?;
            if slash > total {
                return Err(PrecompileError::Fatal(
                    "OCOMP bonded slash exceeds total staked".into(),
                ));
            }
            self.stake_amount.write(&validator, remaining)?;
            self.total_staked.write(total - slash)?;
            if !slash.is_zero() {
                self.storage.decrease_balance(STAKING_ADDRESS, slash)?;
            }
            validators.record_ocomp_bonded_slash(validator, remaining)?;
            slash
        } else {
            U256::ZERO
        };

        guard.commit();
        Ok(OcompMissPenalty {
            first_in_window,
            miss_count,
            recovery_deadline,
            slashed_bonded,
        })
    }

    /// Resolves one recovery window only when its fixed deadline is due. This
    /// narrow seam is also used immediately before recording a same-height new
    /// OCOMP miss, so an expired window cannot suppress the next first slash.
    pub fn resolve_due_ocomp_recovery_window(
        &mut self,
        validator: Address,
    ) -> Result<OcompRecoveryResolution> {
        let guard = self.storage.checkpoint_guard();
        let at_height = self.storage.block_number()?;
        let minimum = self.config_min_stake.read()?;
        let mut validators = ValidatorSet::new(self.storage.clone());
        let Some(window) = validators.ocomp_recovery_window(validator)? else {
            return Ok(OcompRecoveryResolution::NotOpen);
        };
        if at_height < window.recovery_deadline {
            return Ok(OcompRecoveryResolution::NotDue {
                recovery_deadline: window.recovery_deadline,
            });
        }

        let bonded = self.stake_amount.read(&validator)?;
        let lifecycle = validators.validator_lifecycle(validator)?;
        let resolution = if !matches!(lifecycle, ValidatorLifecycle::Active(_)) {
            validators.resolve_ocomp_recovery_window(
                validator,
                window.recovery_deadline,
                bonded,
                OcompRecoveryOutcome::NonActive,
            )?;
            OcompRecoveryResolution::ClosedNonActive {
                recovery_deadline: window.recovery_deadline,
            }
        } else if bonded >= minimum {
            validators.resolve_ocomp_recovery_window(
                validator,
                window.recovery_deadline,
                bonded,
                OcompRecoveryOutcome::Restored,
            )?;
            OcompRecoveryResolution::Restored {
                recovery_deadline: window.recovery_deadline,
            }
        } else {
            let observability =
                validators
                    .jail_validator_deferred(validator)?
                    .ok_or_else(|| {
                        PrecompileError::Fatal(
                            "active OCOMP recovery validator was not jailed".into(),
                        )
                    })?;
            validators.resolve_ocomp_recovery_window(
                validator,
                window.recovery_deadline,
                bonded,
                OcompRecoveryOutcome::Jailed,
            )?;
            OcompRecoveryResolution::Jailed {
                recovery_deadline: window.recovery_deadline,
                observability,
            }
        };
        guard.commit();
        Ok(resolution)
    }

    /// Checks every open OCOMP recovery window once per block. Valid registry
    /// state is bounded by the existing consensus committee/codec maximum. A
    /// corrupt over-bound count fails before address materialization.
    pub fn close_due_ocomp_recovery_windows(&mut self) -> Result<OcompRecoverySweep> {
        let guard = self.storage.checkpoint_guard();
        let current_block = self.storage.block_number()?;
        let validators = ValidatorSet::new(self.storage.clone());
        let validator_count = validators.validator_count()?;
        if validator_count > outbe_validatorset::runtime::CONSENSUS_VALIDATOR_BOUND {
            return Err(PrecompileError::Fatal(format!(
                "validator registry exceeds consensus bound: {validator_count} > {}",
                outbe_validatorset::runtime::CONSENSUS_VALIDATOR_BOUND
            )));
        }
        let addresses = validators.registered_validator_addresses()?;
        let mut log = RecoverySweepLog::default();

        for validator in addresses {
            if validators.ocomp_recovery_window(validator)?.is_none() {
                continue;
            }
            log.count_open_window()?;
            let resolution = self.resolve_due_ocomp_recovery_window(validator)?;
            log.record(validator, resolution)?;
        }

        guard.commit();
        Ok(log.publish(current_block))
    }
}

/// Counts and observations of one recovery sweep. The sweep publishes the
/// observations only after its checkpoint commits.
#[derive(Default)]
struct RecoverySweepLog {
    sweep: OcompRecoverySweep,
    open_deadlines: Vec<(Address, u64)>,
    resolutions: Vec<(Address, &'static str)>,
    punishments: Vec<DeferredValidatorPunishment>,
}

impl RecoverySweepLog {
    fn count_open_window(&mut self) -> Result<()> {
        self.sweep.open_windows = increment(
            self.sweep.open_windows,
            "OCOMP recovery open-window count overflow",
        )?;
        Ok(())
    }

    fn record(&mut self, validator: Address, resolution: OcompRecoveryResolution) -> Result<()> {
        match resolution {
            OcompRecoveryResolution::NotOpen => {}
            OcompRecoveryResolution::NotDue { recovery_deadline } => {
                self.open_deadlines.push((validator, recovery_deadline));
            }
            OcompRecoveryResolution::Restored { .. } => {
                self.sweep.restored =
                    increment(self.sweep.restored, "OCOMP restored count overflow")?;
                self.resolutions.push((validator, "restored"));
            }
            OcompRecoveryResolution::Jailed { observability, .. } => {
                self.sweep.jailed = increment(self.sweep.jailed, "OCOMP jailed count overflow")?;
                self.resolutions.push((validator, "jailed"));
                self.punishments.push(observability);
            }
            OcompRecoveryResolution::ClosedNonActive { .. } => {
                self.sweep.closed_non_active = increment(
                    self.sweep.closed_non_active,
                    "OCOMP non-active close count overflow",
                )?;
                self.resolutions.push((validator, "non_active"));
            }
        }
        Ok(())
    }

    /// Emits the deadline metrics, the deferred punishments, the resolution
    /// metrics and the sweep metric, in that order.
    fn publish(self, current_block: u64) -> OcompRecoverySweep {
        for (validator, recovery_deadline) in self.open_deadlines {
            outbe_validatorset::metrics::record_ocomp_recovery_deadline(
                validator,
                recovery_deadline,
            );
        }
        for punishment in self.punishments {
            punishment.record();
        }
        let sweep = self.sweep;
        let resolved_count = sweep
            .restored
            .saturating_add(sweep.jailed)
            .saturating_add(sweep.closed_non_active);
        for (validator, outcome) in self.resolutions {
            outbe_validatorset::metrics::record_ocomp_recovery_resolution(validator, outcome);
        }
        outbe_validatorset::metrics::record_ocomp_recovery_sweep(
            current_block,
            sweep.open_windows.saturating_sub(resolved_count),
        );
        sweep
    }
}

/// Adds one to a sweep count. An overflow is a fatal state error.
fn increment(count: u32, overflow: &'static str) -> Result<u32> {
    count
        .checked_add(1)
        .ok_or_else(|| PrecompileError::Fatal(overflow.into()))
}
