use super::{registered_status, status};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorHistory, ValidatorLifecycle, ValidatorState};
use alloy_primitives::{Address, B256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use std::collections::HashSet;
use tracing::{info, warn};

impl ValidatorSet<'_> {
    /// Applies a validated boundary and its certified TEE-expiry exclusions.
    pub(crate) fn activate_validated_boundary_set_with_expiry_exclusions(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        freeze_height: u64,
        tee_expired_target_exclusions: &[Address],
    ) -> Result<()> {
        if tee_expired_target_exclusions.is_empty() {
            return self.apply_reshared_set(new_active_set, active_set_hash, freeze_height, &[]);
        }
        if tee_expired_target_exclusions.len()
            > outbe_primitives::validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS
        {
            return Err(PrecompileError::Fatal(format!(
                "TEE expiry exclusions exceed protocol cap: {} > {}",
                tee_expired_target_exclusions.len(),
                outbe_primitives::validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS
            )));
        }
        let mut unique_exclusions = std::collections::BTreeSet::new();
        for address in tee_expired_target_exclusions {
            if address.is_zero() || !unique_exclusions.insert(*address) {
                return Err(PrecompileError::Fatal(
                    "TEE expiry exclusions must contain unique non-zero validators".into(),
                ));
            }
            if new_active_set.contains(address) {
                return Err(PrecompileError::Fatal(format!(
                    "TEE-expired validator {address} is also present in the new active set"
                )));
            }
            if self.address_to_index.read(address)? == 0 {
                return Err(PrecompileError::Fatal(format!(
                    "TEE expiry exclusions contain unregistered validator {address}"
                )));
            }
        }
        self.apply_reshared_set(
            new_active_set,
            active_set_hash,
            freeze_height,
            tee_expired_target_exclusions,
        )
    }

    fn apply_reshared_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        freeze_height: u64,
        tee_expired_target_exclusions: &[Address],
    ) -> Result<()> {
        let active_count: u32 = new_active_set
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active set count exceeds u32".into()))?;
        let plan =
            self.plan_boundary(new_active_set, freeze_height, tee_expired_target_exclusions)?;
        plan.ensure_participants_match(new_active_set)?;
        let pending = plan.pending_set_change();
        self.commit_boundary(&plan, active_set_hash, active_count, pending)?;
        self.publish_boundary(&plan, active_set_hash, active_count, pending);
        Ok(())
    }

    /// Plans the entire state transition before the first write.
    ///
    /// The executor validates canonical Commonware order and the address hash
    /// against the incoming snapshot. This layer validates unique membership
    /// and lifecycle eligibility.
    fn plan_boundary(
        &self,
        new_active_set: &[Address],
        freeze_height: u64,
        tee_expired_target_exclusions: &[Address],
    ) -> Result<BoundaryPlan> {
        let addresses = self.registered_validator_addresses()?;
        let mut states = Vec::with_capacity(addresses.len());
        for addr in addresses {
            states.push(self.validator_state(addr)?);
        }

        let mut plan = BoundaryPlan::with_capacity(states.len());
        for before in states {
            let included = new_active_set.contains(&before.address());
            let (lifecycle, effect) = if tee_expired_target_exclusions.contains(&before.address()) {
                tee_expired_lifecycle(&before, included)?
            } else if included {
                (self.included_lifecycle(&before, freeze_height)?, None)
            } else {
                omitted_lifecycle(&before, freeze_height)?
            };
            if let Some(effect) = effect {
                plan.record_effect(effect, before.address());
            }
            let after = before.clone().with_lifecycle(lifecycle)?;
            plan.transitions.push((before, after));
        }
        Ok(plan)
    }

    /// The lifecycle of a validator that the new active set includes.
    fn included_lifecycle(
        &self,
        before: &ValidatorState,
        freeze_height: u64,
    ) -> Result<ValidatorLifecycle> {
        match before.lifecycle().clone() {
            ValidatorLifecycle::Joining(joining) => {
                if self.ocomp_registration(before.address())?.is_none() {
                    return Err(PrecompileError::Fatal(format!(
                        "certified active set contains validator {} without OCOMP admission",
                        before.address()
                    )));
                }
                Ok(ValidatorLifecycle::Active(
                    state_machine::activate_at_boundary(joining),
                ))
            }
            ValidatorLifecycle::Active(active) => Ok(ValidatorLifecycle::Active(
                state_machine::retain_active_at_boundary(active),
            )),
            ValidatorLifecycle::Exiting(exiting) => {
                let changed_at = exiting_deactivation_height(before)?;
                if changed_at <= freeze_height {
                    return Err(PrecompileError::Fatal(format!(
                        "validated boundary retained validator {} that exited at {changed_at} before freeze {freeze_height}",
                        before.address()
                    )));
                }
                Ok(ValidatorLifecycle::Exiting(exiting))
            }
            ValidatorLifecycle::JailRetained(jailed) => {
                let jailed_at = before.stored_jailed_at();
                if jailed_at <= freeze_height {
                    return Err(PrecompileError::Fatal(format!(
                        "validated boundary retained validator {} jailed at {jailed_at} before freeze {freeze_height}",
                        before.address()
                    )));
                }
                Ok(ValidatorLifecycle::JailRetained(jailed))
            }
            ValidatorLifecycle::WaitingForStake(waiting) => Ok(ValidatorLifecycle::Exiting(
                state_machine::exit_waiting_for_stake_at_boundary(
                    waiting,
                    demotion_height(before, freeze_height)?,
                )?,
            )),
            ValidatorLifecycle::WaitingForReadiness(waiting) => Ok(ValidatorLifecycle::Exiting(
                state_machine::exit_waiting_for_readiness_at_boundary(
                    waiting,
                    demotion_height(before, freeze_height)?,
                )?,
            )),
            lifecycle => Err(PrecompileError::Fatal(format!(
                "validated boundary included ineligible validator {} with status {}",
                before.address(),
                registered_status(&lifecycle)?
            ))),
        }
    }

    /// Writes the planned transitions, the active-set hash, the pending flag
    /// and the set-update event.
    fn commit_boundary(
        &mut self,
        plan: &BoundaryPlan,
        active_set_hash: B256,
        active_count: u32,
        pending: bool,
    ) -> Result<()> {
        // The planner performs every fallible semantic check before this
        // checkpoint. Storage writes, hash, repair flag, and event commit as one
        // bundle even for direct legacy calls.
        let guard = self.storage.checkpoint_guard();
        for (before, after) in &plan.transitions {
            self.persist_validator_state_delta(before, after)?;
        }
        self.active_consensus_set_hash.write(active_set_hash)?;
        self.pending_set_change.write(pending)?;
        self.emit(IValidatorSet::ConsensusSetUpdated {
            activeCount: active_count,
        })?;
        guard.commit();
        Ok(())
    }

    /// Records the metrics, journal entries and logs of a committed boundary.
    fn publish_boundary(
        &self,
        plan: &BoundaryPlan,
        active_set_hash: B256,
        active_count: u32,
        pending: bool,
    ) {
        plan.record_metrics(active_count, pending);
        let block_number = self.storage.block_number().unwrap_or(0);
        plan.record_journal(block_number, active_count, pending, active_set_hash);
        let (active, exiting, unbonding) = plan.status_counts();
        crate::metrics::record_aggregate_status_counts(active, exiting, unbonding);
        plan.log_activation(block_number, active_count, pending, active_set_hash);
        self.log_tee_expiry(plan);
    }

    /// Logs every TEE-expiry demotion and readiness reset of a boundary.
    fn log_tee_expiry(&self, plan: &BoundaryPlan) {
        for addr in &plan.tee_expired_active {
            warn!(
                target: "outbe::validatorset",
                event = "validator_tee_expired_demoted",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "certified freeze-height TEE expiry demoted ACTIVE validator to PENDING"
            );
        }
        for addr in &plan.tee_expired_pending {
            warn!(
                target: "outbe::validatorset",
                event = "validator_tee_expired_readiness_cleared",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "certified freeze-height TEE expiry cleared PENDING validator readiness"
            );
        }
    }
}

/// The planned boundary: one before/after pair per registered validator, in
/// registry order, and the validators that each boundary effect touches.
struct BoundaryPlan {
    transitions: Vec<(ValidatorState, ValidatorState)>,
    transitioned_to_unbonding: Vec<Address>,
    tee_expired_active: Vec<Address>,
    tee_expired_pending: Vec<Address>,
}

/// A boundary effect that the metrics, journal and logs report per validator.
#[derive(Clone, Copy)]
enum BoundaryEffect {
    Unbonding,
    TeeExpiredActive,
    TeeExpiredPending,
}

impl BoundaryPlan {
    fn with_capacity(validators: usize) -> Self {
        Self {
            transitions: Vec::with_capacity(validators),
            transitioned_to_unbonding: Vec::new(),
            tee_expired_active: Vec::new(),
            tee_expired_pending: Vec::new(),
        }
    }

    fn record_effect(&mut self, effect: BoundaryEffect, address: Address) {
        match effect {
            BoundaryEffect::Unbonding => self.transitioned_to_unbonding.push(address),
            BoundaryEffect::TeeExpiredActive => self.tee_expired_active.push(address),
            BoundaryEffect::TeeExpiredPending => self.tee_expired_pending.push(address),
        }
    }

    /// Requires the planned consensus participants to be exactly the unique
    /// members of the certified active set.
    fn ensure_participants_match(&self, new_active_set: &[Address]) -> Result<()> {
        let planned_participants: Vec<_> = self
            .transitions
            .iter()
            .filter_map(|(_, after)| {
                after
                    .lifecycle()
                    .is_current_consensus_participant()
                    .then_some(after.address())
            })
            .collect();
        let unique_artifact_members: HashSet<_> = new_active_set.iter().copied().collect();
        if unique_artifact_members.len() != new_active_set.len()
            || planned_participants.len() != new_active_set.len()
            || planned_participants
                .iter()
                .any(|address| !unique_artifact_members.contains(address))
        {
            return Err(PrecompileError::Fatal(format!(
                "validated boundary participant membership mismatch: planned {planned_participants:?}, artifact {new_active_set:?}"
            )));
        }
        Ok(())
    }

    /// Whether a planned validator still waits for a later set change.
    fn pending_set_change(&self) -> bool {
        self.transitions.iter().any(|(_, after)| {
            matches!(
                after.lifecycle(),
                ValidatorLifecycle::WaitingForReadiness(_)
                    | ValidatorLifecycle::Joining(_)
                    | ValidatorLifecycle::Exiting(_)
                    | ValidatorLifecycle::JailRetained(_)
            )
        })
    }

    /// The planned ACTIVE, EXITING and UNBONDING counts.
    fn status_counts(&self) -> (usize, usize, usize) {
        let mut active = 0usize;
        let mut exiting = 0usize;
        let mut unbonding = 0usize;
        for (_, after) in &self.transitions {
            match after.lifecycle() {
                ValidatorLifecycle::Active(_) => active += 1,
                ValidatorLifecycle::Exiting(_) => exiting += 1,
                ValidatorLifecycle::Unbonding(_) => unbonding += 1,
                _ => {}
            }
        }
        (active, exiting, unbonding)
    }

    fn record_metrics(&self, active_count: u32, pending: bool) {
        crate::metrics::record_reshared_set_activated(
            active_count,
            self.transitioned_to_unbonding.len(),
        );
        crate::metrics::record_pending_set_change(pending);
        for (_, after) in &self.transitions {
            if let Some(stored_status) = after.stored_status() {
                crate::metrics::record_validator_status(after.address(), stored_status);
            }
        }
        for addr in &self.tee_expired_active {
            crate::metrics::record_validator_status(*addr, status::PENDING);
            crate::metrics::record_validator_tee_expiry(*addr, "active_demoted");
        }
        for addr in &self.tee_expired_pending {
            crate::metrics::record_validator_tee_expiry(*addr, "pending_cleared");
        }
        crate::metrics::record_tee_expiry_exclusions(
            self.tee_expired_active.len(),
            self.tee_expired_pending.len(),
        );
    }

    fn record_journal(
        &self,
        block_number: u64,
        active_count: u32,
        pending: bool,
        active_set_hash: B256,
    ) {
        journal_record(JournalRecord::ResharedSetActivated {
            wall_clock: iso8601_now(),
            block_number,
            active_count,
            transitioned_to_unbonding: self.transitioned_to_unbonding.len() as u64,
            pending_set_change: pending,
            active_set_hash: format!("{active_set_hash:?}"),
        });
        for addr in &self.transitioned_to_unbonding {
            journal_record(JournalRecord::ValidatorUnbonding {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{addr:?}"),
            });
        }
    }

    fn log_activation(
        &self,
        block_number: u64,
        active_count: u32,
        pending: bool,
        active_set_hash: B256,
    ) {
        info!(
            target: "outbe::validatorset",
            event = "reshared_set_activated",
            active_count,
            transitioned_to_unbonding = self.transitioned_to_unbonding.len(),
            pending_set_change = pending,
            block_number,
            active_set_hash = %active_set_hash,
            "DKG reshare activated; new active set committed",
        );
        for addr in &self.transitioned_to_unbonding {
            info!(
                target: "outbe::validatorset",
                event = "validator_unbonding",
                validator = %addr,
                block_number,
                "validator transitioned EXITING -> UNBONDING (excluded from new set)",
            );
        }
    }
}

/// The lifecycle of a validator in the certified TEE-expiry exclusions.
fn tee_expired_lifecycle(
    before: &ValidatorState,
    included: bool,
) -> Result<(ValidatorLifecycle, Option<BoundaryEffect>)> {
    match (before.lifecycle().clone(), included) {
        (ValidatorLifecycle::Active(active), false) => Ok((
            ValidatorLifecycle::WaitingForReadiness(state_machine::expire_active_tee(active)),
            Some(BoundaryEffect::TeeExpiredActive),
        )),
        (ValidatorLifecycle::Joining(joining), false) => Ok((
            ValidatorLifecycle::WaitingForReadiness(state_machine::expire_joining_tee(joining)),
            Some(BoundaryEffect::TeeExpiredPending),
        )),
        (ValidatorLifecycle::WaitingForReadiness(waiting), false) => Ok((
            ValidatorLifecycle::WaitingForReadiness(waiting),
            Some(BoundaryEffect::TeeExpiredPending),
        )),
        (lifecycle, _) => Err(PrecompileError::Fatal(format!(
            "TEE expiry exclusion contains validator {} with ineligible status {}",
            before.address(),
            registered_status(&lifecycle)?
        ))),
    }
}

/// The lifecycle of a validator that the new active set omits.
fn omitted_lifecycle(
    before: &ValidatorState,
    freeze_height: u64,
) -> Result<(ValidatorLifecycle, Option<BoundaryEffect>)> {
    match before.lifecycle().clone() {
        ValidatorLifecycle::Active(_) => Err(PrecompileError::Fatal(format!(
            "validated boundary omitted active validator {}",
            before.address()
        ))),
        ValidatorLifecycle::Exiting(exiting) => {
            let changed_at = exiting_deactivation_height(before)?;
            if changed_at > freeze_height {
                return Err(PrecompileError::Fatal(format!(
                    "validated boundary omitted validator {} that exited at {changed_at} after freeze {freeze_height}",
                    before.address()
                )));
            }
            Ok((
                ValidatorLifecycle::Unbonding(state_machine::exclude_exiting_at_boundary(exiting)),
                Some(BoundaryEffect::Unbonding),
            ))
        }
        ValidatorLifecycle::JailRetained(jailed) => {
            let jailed_at = before.stored_jailed_at();
            if jailed_at > freeze_height {
                return Err(PrecompileError::Fatal(format!(
                    "validated boundary omitted validator {} jailed at {jailed_at} after freeze {freeze_height}",
                    before.address()
                )));
            }
            Ok((
                ValidatorLifecycle::Jail(state_machine::exclude_jailed_at_boundary(jailed)),
                None,
            ))
        }
        lifecycle => Ok((lifecycle, None)),
    }
}

/// The last deactivation height of a validator, if its history records one.
fn last_deactivation_height(before: &ValidatorState) -> Option<u64> {
    before
        .history()
        .and_then(ValidatorHistory::last_deactivated_at_height)
}

/// The deactivation height that every exiting validator must record.
fn exiting_deactivation_height(before: &ValidatorState) -> Result<u64> {
    last_deactivation_height(before).ok_or_else(|| {
        PrecompileError::Fatal(format!(
            "exiting validator {} has no deactivation height",
            before.address()
        ))
    })
}

/// The demotion height of an included validator that is no longer `Joining`.
fn demotion_height(before: &ValidatorState, freeze_height: u64) -> Result<u64> {
    post_freeze_demotion_height(
        last_deactivation_height(before),
        freeze_height,
        before.address(),
        registered_status(before.lifecycle())?,
    )
}

/// Height used to retain a frozen joiner who is no longer `Joining`.
///
/// A height after the freeze is the demotion block. A missing height is an
/// already-finalized demotion from before that stamp existed: `freeze + 1`
/// stays inside this epoch and before the next freeze. A height at or before
/// the freeze means the artifact included someone who was already ineligible.
fn post_freeze_demotion_height(
    changed_at: Option<u64>,
    freeze_height: u64,
    address: Address,
    status: u8,
) -> Result<u64> {
    match changed_at {
        Some(height) if height > freeze_height => Ok(height),
        Some(_) => Err(PrecompileError::Fatal(format!(
            "validated boundary included ineligible validator {address} with status {status}"
        ))),
        None => {
            let height = freeze_height.saturating_add(1);
            if height <= freeze_height {
                return Err(PrecompileError::Fatal(
                    "validator deactivation height must be non-zero".into(),
                ));
            }
            Ok(height)
        }
    }
}
