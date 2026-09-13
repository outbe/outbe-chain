use super::{registered_status, status};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorHistory, ValidatorLifecycle};
use alloy_primitives::Address;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use tracing::{info, warn};

/// Non-transactional observability produced by a committed punitive validator
/// transition. Callers that wrap the transition in a wider checkpoint defer
/// this report until that outer checkpoint commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredValidatorPunishment {
    addr: Address,
    target: u8,
    target_label: &'static str,
    jailed: bool,
    block_number: u64,
}

impl DeferredValidatorPunishment {
    pub fn record(self) {
        crate::metrics::record_validator_status(self.addr, self.target);
        crate::metrics::record_validator_force_exit(self.addr);
        crate::metrics::record_pending_set_change(true);
        journal_record(JournalRecord::ValidatorForcedExit {
            wall_clock: iso8601_now(),
            block_number: self.block_number,
            validator: format!("{:?}", self.addr),
            status_before: "ACTIVE".into(),
            status_after: self.target_label.into(),
        });
        warn!(
            target: "outbe::validatorset",
            event = if self.jailed { "validator_jailed" } else { "validator_force_exit" },
            validator = %self.addr,
            status_after = self.target_label,
            block_number = self.block_number,
            "validator punished from ACTIVE (force-exit/jail)",
        );
    }
}

impl ValidatorSet<'_> {
    /// Deactivates a validator - transitions to EXITING (awaiting DKG reshare to exclude).
    ///
    /// The caller must be the config owner or the validator itself.
    pub fn deactivate_validator(&mut self, caller: Address, addr: Address) -> Result<()> {
        let owner = self.config_owner.read()?;
        if caller != owner && caller != addr {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        let before = self.validator_state(addr)?;
        let active = match before.lifecycle().clone() {
            ValidatorLifecycle::Active(active) => active,
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()))
            }
            _ => {
                return Err(PrecompileError::Revert(
                    "can only deactivate an active validator".into(),
                ))
            }
        };
        let height = self.storage.block_number()?;
        let stake = *before.stake().ok_or_else(|| {
            PrecompileError::Fatal("active validator is missing stake projection".into())
        })?;
        let lifecycle =
            ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, height)?);
        let after = before.clone().with_lifecycle(lifecycle)?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;

        // Signal pending set change so consensus triggers DKG reshare to exclude
        self.pending_set_change.write(true)?;

        self.emit(IValidatorSet::ValidatorDeactivated {
            validator: addr,
            atHeight: height,
        })?;
        guard.commit();

        crate::metrics::record_validator_status(addr, status::EXITING);
        crate::metrics::record_validator_deactivate(addr);
        crate::metrics::record_pending_set_change(true);

        journal_record(JournalRecord::ValidatorDeactivated {
            wall_clock: iso8601_now(),
            block_number: height,
            validator: format!("{addr:?}"),
            caller: format!("{caller:?}"),
            self_initiated: caller == addr,
        });

        info!(
            target: "outbe::validatorset",
            event = "validator_deactivated",
            validator = %addr,
            %caller,
            self_initiated = (caller == addr),
            block_number = height,
            "validator transitioned ACTIVE -> EXITING (voluntary deactivation)",
        );

        Ok(())
    }

    /// Forces a validator out of consensus because of a severe fault.
    ///
    /// The validator enters EXITING and is removed from consensus on the next
    /// successful reshare. Stake withdrawal is handled by Staking after the
    /// validator reaches UNBONDING.
    pub fn force_exit_validator(&mut self, addr: Address) -> Result<()> {
        if let Some(observability) = self.punish_validator(addr, false)? {
            observability.record();
        }
        Ok(())
    }

    /// Jails a validator for a severe consensus/oracle fault (felony). Unlike
    /// [`Self::force_exit_validator`], the validator is NOT removed from the
    /// registry: it is frozen in JAILED, excluded from the next reshare target
    /// (so the reshare clears its share), and may later return via
    /// `unjailValidator` (`Jail -> WaitingForReadiness -> Joining -> Active`) or,
    /// after boundary exclusion, leave via a full unstake
    /// (`Jail -> Unbonding -> Inactive`). The slash itself is applied by the caller
    /// AFTER this call (`slash_stake` leaves a jailed lifecycle untouched).
    /// Increments `slash_count` once. Repeated punishment of the same lifecycle
    /// is a no-op even if a caller bypasses SlashIndicator's replay guard.
    pub fn jail_validator(&mut self, addr: Address) -> Result<()> {
        if let Some(observability) = self.punish_validator(addr, true)? {
            observability.record();
        }
        Ok(())
    }

    /// Applies the jail transition but leaves metrics, journal and tracing to
    /// the caller so a wider atomic transition cannot publish rolled-back state.
    pub fn jail_validator_deferred(
        &mut self,
        addr: Address,
    ) -> Result<Option<DeferredValidatorPunishment>> {
        self.punish_validator(addr, true)
    }

    /// Jails an ACTIVE validator whose canonical TEE lease reached its deadline.
    ///
    /// This is an availability/safety transition, not a slash: bonded stake and
    /// `slash_count` are preserved exactly. The old committee share remains
    /// accountable in `JailRetained` until the normal validated boundary removes
    /// it. Re-execution after the first transition is an idempotent no-op.
    pub fn jail_validator_for_tee_expiry(&mut self, addr: Address) -> Result<bool> {
        let before = self.validator_state(addr)?;
        let active = match before.lifecycle().clone() {
            ValidatorLifecycle::Active(active) => active,
            ValidatorLifecycle::JailRetained(_) | ValidatorLifecycle::Jail(_) => {
                return Ok(false);
            }
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            lifecycle => {
                return Err(PrecompileError::Fatal(format!(
                    "TEE expiry sweep selected validator {addr} with ineligible status {}",
                    registered_status(&lifecycle)?
                )));
            }
        };
        let block_number = self.storage.block_number()?;
        let next = ValidatorLifecycle::JailRetained(state_machine::jail(active, block_number)?);
        let after = before.clone().with_lifecycle(next)?;

        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        self.pending_set_change.write(true)?;
        self.emit(IValidatorSet::ValidatorJailed {
            validator: addr,
            atHeight: block_number,
        })?;
        guard.commit();

        crate::metrics::record_validator_status(addr, status::JAILED);
        crate::metrics::record_validator_tee_expiry(addr, "deadline_jailed");
        crate::metrics::record_pending_set_change(true);
        warn!(
            target: "outbe::validatorset",
            event = "validator_tee_lease_expired",
            validator = %addr,
            block_number,
            "validator jailed without slashing after TEE lease deadline",
        );
        Ok(true)
    }

    /// Shared punitive transition for [`Self::force_exit_validator`] (`jail =
    /// false` -> ACTIVE->EXITING, the validator leaves the registry via UNBONDING)
    /// and [`Self::jail_validator`] (`jail = true` -> ACTIVE->JAILED, the validator
    /// is frozen in the registry). Both signal a reshare and bump `slash_count`
    /// exactly once.
    fn punish_validator(
        &mut self,
        addr: Address,
        jail: bool,
    ) -> Result<Option<DeferredValidatorPunishment>> {
        let before = self.validator_state(addr)?;
        let lifecycle = before.lifecycle().clone();
        if matches!(lifecycle, ValidatorLifecycle::Absent) {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        let current_status = registered_status(&lifecycle)?;
        let block_number = self.storage.block_number()?;
        let (target, target_label, action) = if jail {
            (status::JAILED, "JAILED", "jail")
        } else {
            (status::EXITING, "EXITING", "force-exit")
        };

        let history = before.history().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let active = match lifecycle {
            ValidatorLifecycle::Active(active) => active,
            ValidatorLifecycle::JailRetained(_) | ValidatorLifecycle::Jail(_) if jail => {
                return Ok(None)
            }
            ValidatorLifecycle::Exiting(_)
            | ValidatorLifecycle::Unbonding(_)
            | ValidatorLifecycle::Inactive(_) => return Ok(None),
            _ => {
                return Err(PrecompileError::Revert(format!(
                    "cannot {action} validator with status {current_status}: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed"
                )));
            }
        };
        let stake = *before.stake().ok_or_else(|| {
            PrecompileError::Fatal("active validator is missing stake projection".into())
        })?;
        let next = if jail {
            ValidatorLifecycle::JailRetained(state_machine::jail(active, block_number)?)
        } else {
            ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, block_number)?)
        };
        let next = state_machine::with_history(
            next,
            ValidatorHistory::new(
                history.joined_at_height(),
                Some(block_number),
                history
                    .slash_count()
                    .checked_add(1)
                    .ok_or_else(|| PrecompileError::Fatal("slash count overflow".into()))?,
                history.missed_blocks(),
                history.missed_votes(),
                history.blocks_proposed(),
            ),
        )?;
        let after = before.clone().with_lifecycle(next)?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;

        self.pending_set_change.write(true)?;
        if jail {
            self.emit(IValidatorSet::ValidatorJailed {
                validator: addr,
                atHeight: block_number,
            })?;
        } else {
            self.emit(IValidatorSet::ValidatorDeactivated {
                validator: addr,
                atHeight: block_number,
            })?;
            self.emit(IValidatorSet::ValidatorForcedExit {
                validator: addr,
                atHeight: block_number,
            })?;
        }
        guard.commit();

        Ok(Some(DeferredValidatorPunishment {
            addr,
            target,
            target_label,
            jailed: jail,
            block_number,
        }))
    }

    /// Unjails a JAILED validator back to PENDING. Called by Staking's
    /// `unjailValidator` (which first verifies the caller's stake >= min_stake);
    /// the caller must be the validator itself. Enforces the unjail cooldown,
    /// clears missed-block/vote counters, and signals a reshare. Identity,
    /// deactivation, slash and proposal history remain intact.
    pub fn unjail_after_stake_check(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        let jailed = match before.lifecycle().clone() {
            ValidatorLifecycle::Jail(jailed) => jailed,
            ValidatorLifecycle::JailRetained(_) => {
                return Err(PrecompileError::Revert(
                    "jailed validator is still retained in the current committee".into(),
                ));
            }
            ValidatorLifecycle::Absent => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "unjailValidator requires JAILED status, got {}",
                    registered_status(&lifecycle)?
                )));
            }
        };
        let block_number = self.storage.block_number()?;
        let cooldown = self.unjail_cooldown_blocks()?;
        // Staking checked its authoritative minimum before entering this facade.
        let pending = state_machine::unjail(jailed, block_number, cooldown, before.bonded_stake())?;
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::WaitingForReadiness(pending))?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;

        // Re-joining requires a fresh readiness confirmation before DKG.
        self.pending_set_change.write(true)?;

        self.emit(IValidatorSet::ValidatorUnjailed {
            validator: addr,
            atHeight: block_number,
        })?;
        guard.commit();

        crate::metrics::record_validator_status(addr, status::PENDING);
        crate::metrics::record_pending_set_change(true);
        Ok(())
    }

    /// Unjail cooldown in blocks (default 0 - immediate unjail allowed).
    pub fn unjail_cooldown_blocks(&self) -> Result<u64> {
        self.config_unjail_cooldown_blocks.read()
    }
}
