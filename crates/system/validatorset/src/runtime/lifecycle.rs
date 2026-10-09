use super::{registered_status, status};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{
    self, Active, HistoryCounters, ValidatorHistory, ValidatorLifecycle, ValidatorState,
};
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
    /// Deactivates a validator. The validator transitions to EXITING and awaits a
    /// DKG reshare that excludes it.
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
        // Signal pending set change so consensus triggers DKG reshare to exclude
        self.commit_set_change(&before, &after, |vs| {
            vs.emit(IValidatorSet::ValidatorDeactivated {
                validator: addr,
                atHeight: height,
            })
        })?;

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
    /// The validator enters EXITING. The next successful reshare removes it from
    /// consensus. Staking handles stake withdrawal after the validator reaches
    /// UNBONDING.
    pub fn force_exit_validator(&mut self, addr: Address) -> Result<()> {
        if let Some(observability) = self.punish_validator(addr, false)? {
            observability.record();
        }
        Ok(())
    }

    /// Jails a validator for a severe consensus/oracle fault (felony).
    ///
    /// Unlike [`Self::force_exit_validator`], this does NOT remove the validator
    /// from the registry. The validator is frozen in JAILED and excluded from the
    /// next reshare target, so the reshare clears its share. Later, the validator
    /// may:
    ///
    /// - return via `unjailValidator` (`Jail -> WaitingForReadiness -> Joining -> Active`),
    ///   or
    /// - after boundary exclusion, leave via a full unstake
    ///   (`Jail -> Unbonding -> Inactive`).
    ///
    /// The caller applies the slash itself AFTER this call (`slash_stake` leaves a
    /// jailed lifecycle untouched). Increments `slash_count` once. Repeated
    /// punishment of the same lifecycle is a no-op even if a caller bypasses
    /// SlashIndicator's replay guard.
    pub fn jail_validator(&mut self, addr: Address) -> Result<()> {
        if let Some(observability) = self.punish_validator(addr, true)? {
            observability.record();
        }
        Ok(())
    }

    /// Applies the jail transition but leaves metrics, journal and tracing to
    /// the caller. A wider atomic transition then cannot publish rolled-back state.
    pub fn jail_validator_deferred(
        &mut self,
        addr: Address,
    ) -> Result<Option<DeferredValidatorPunishment>> {
        self.punish_validator(addr, true)
    }

    /// Jails an ACTIVE validator whose canonical TEE lease reached its deadline.
    ///
    /// This is an availability/safety transition, not a slash. It preserves bonded
    /// stake and `slash_count` exactly. The old committee share remains
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
        self.commit_set_change(&before, &after, |vs| {
            vs.emit(IValidatorSet::ValidatorJailed {
                validator: addr,
                atHeight: block_number,
            })
        })?;

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

    /// Shared punitive transition for two callers:
    ///
    /// - [`Self::force_exit_validator`] (`jail = false` -> ACTIVE->EXITING). The
    ///   validator leaves the registry via UNBONDING.
    /// - [`Self::jail_validator`] (`jail = true` -> ACTIVE->JAILED). The validator
    ///   is frozen in the registry.
    ///
    /// Both signal a reshare and bump `slash_count` exactly once.
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
        let punishment = Punishment {
            jail,
            block_number: self.storage.block_number()?,
        };
        let history = before.history().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let Some(active) = punishable_active(lifecycle, punishment, current_status)? else {
            return Ok(None);
        };
        let after = punished_state(&before, active, history, punishment)?;
        self.commit_set_change(&before, &after, |vs| vs.emit_punishment(addr, punishment))?;
        Ok(Some(punishment.deferred(addr)))
    }

    /// Emits the events of a committed punishment: `ValidatorJailed` for a
    /// jail, else `ValidatorDeactivated` and then `ValidatorForcedExit`.
    fn emit_punishment(&mut self, addr: Address, punishment: Punishment) -> Result<()> {
        let block_number = punishment.block_number;
        if punishment.jail {
            return self.emit(IValidatorSet::ValidatorJailed {
                validator: addr,
                atHeight: block_number,
            });
        }
        self.emit(IValidatorSet::ValidatorDeactivated {
            validator: addr,
            atHeight: block_number,
        })?;
        self.emit(IValidatorSet::ValidatorForcedExit {
            validator: addr,
            atHeight: block_number,
        })
    }

    /// Unjails a JAILED validator back to PENDING. Staking's `unjailValidator`
    /// calls this function after it first verifies the caller's stake >= min_stake.
    /// The caller must be the validator itself. This function:
    ///
    /// - enforces the unjail cooldown,
    /// - clears missed-block/vote counters,
    /// - signals a reshare.
    ///
    /// Identity, deactivation, slash and proposal history remain intact.
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
        // Re-joining requires a fresh readiness confirmation before DKG.
        self.commit_set_change(&before, &after, |vs| {
            vs.emit(IValidatorSet::ValidatorUnjailed {
                validator: addr,
                atHeight: block_number,
            })
        })?;

        crate::metrics::record_validator_status(addr, status::PENDING);
        crate::metrics::record_pending_set_change(true);
        Ok(())
    }

    /// Unjail cooldown in blocks (default 0 - immediate unjail allowed).
    pub fn unjail_cooldown_blocks(&self) -> Result<u64> {
        self.config_unjail_cooldown_blocks.read()
    }

    /// Persists a lifecycle transition that changes the consensus set. One
    /// checkpoint holds the changed fields, the raised set-change flag and the
    /// events that `emit` writes.
    fn commit_set_change(
        &mut self,
        before: &ValidatorState,
        after: &ValidatorState,
        emit: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(before, after)?;
        self.pending_set_change.write(true)?;
        emit(self)?;
        guard.commit();
        Ok(())
    }
}

/// A punitive transition from ACTIVE: a jail or a forced exit at
/// `block_number`.
#[derive(Clone, Copy)]
struct Punishment {
    jail: bool,
    block_number: u64,
}

impl Punishment {
    /// The deferred observability of this committed punishment.
    fn deferred(self, addr: Address) -> DeferredValidatorPunishment {
        let (target, target_label) = if self.jail {
            (status::JAILED, "JAILED")
        } else {
            (status::EXITING, "EXITING")
        };
        DeferredValidatorPunishment {
            addr,
            target,
            target_label,
            jailed: self.jail,
            block_number: self.block_number,
        }
    }
}

/// The ACTIVE payload that a punishment transitions, or `None` when the
/// lifecycle already left the committee and the punishment is a no-op.
fn punishable_active(
    lifecycle: ValidatorLifecycle,
    punishment: Punishment,
    current_status: u8,
) -> Result<Option<Active>> {
    match lifecycle {
        ValidatorLifecycle::Active(active) => Ok(Some(active)),
        ValidatorLifecycle::JailRetained(_) | ValidatorLifecycle::Jail(_) if punishment.jail => {
            Ok(None)
        }
        ValidatorLifecycle::Exiting(_)
        | ValidatorLifecycle::Unbonding(_)
        | ValidatorLifecycle::Inactive(_) => Ok(None),
        _ => {
            let action = if punishment.jail {
                "jail"
            } else {
                "force-exit"
            };
            Err(PrecompileError::Revert(format!(
                "cannot {action} validator with status {current_status}: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed"
            )))
        }
    }
}

/// The punished state of an ACTIVE validator: JAILED or EXITING at
/// `block_number`, with one more slash and that deactivation height.
fn punished_state(
    before: &ValidatorState,
    active: Active,
    history: ValidatorHistory,
    punishment: Punishment,
) -> Result<ValidatorState> {
    let block_number = punishment.block_number;
    let stake = *before.stake().ok_or_else(|| {
        PrecompileError::Fatal("active validator is missing stake projection".into())
    })?;
    let next = if punishment.jail {
        ValidatorLifecycle::JailRetained(state_machine::jail(active, block_number)?)
    } else {
        ValidatorLifecycle::Exiting(state_machine::begin_exit(active, stake, block_number)?)
    };
    let next = state_machine::with_history(
        next,
        ValidatorHistory::new(
            history.joined_at_height(),
            Some(block_number),
            HistoryCounters {
                slash_count: history
                    .slash_count()
                    .checked_add(1)
                    .ok_or_else(|| PrecompileError::Fatal("slash count overflow".into()))?,
                missed_blocks: history.missed_blocks(),
                missed_votes: history.missed_votes(),
                blocks_proposed: history.blocks_proposed(),
            },
        ),
    )?;
    before.clone().with_lifecycle(next)
}
