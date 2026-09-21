use super::{registered_status, status};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, ValidatorHistory, ValidatorLifecycle};
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
        let addresses = self.registered_validator_addresses()?;
        let mut states = Vec::with_capacity(addresses.len());
        for addr in addresses {
            states.push(self.validator_state(addr)?);
        }

        // Plan the entire state transition before the first write. Canonical
        // Commonware order and the address hash are validated by the executor
        // against the incoming snapshot; this layer validates unique membership
        // and lifecycle eligibility.
        let mut transitions = Vec::with_capacity(states.len());
        let mut transitioned_to_unbonding = Vec::new();
        let mut tee_expired_active = Vec::new();
        let mut tee_expired_pending = Vec::new();
        for before in states {
            let included = new_active_set.contains(&before.address());
            let tee_expired = tee_expired_target_exclusions.contains(&before.address());
            let changed_at = before
                .history()
                .and_then(ValidatorHistory::last_deactivated_at_height);
            let lifecycle = match (before.lifecycle().clone(), included, tee_expired) {
                (ValidatorLifecycle::Active(active), false, true) => {
                    tee_expired_active.push(before.address());
                    ValidatorLifecycle::WaitingForReadiness(state_machine::expire_active_tee(
                        active,
                    ))
                }
                (ValidatorLifecycle::Joining(joining), false, true) => {
                    tee_expired_pending.push(before.address());
                    ValidatorLifecycle::WaitingForReadiness(state_machine::expire_joining_tee(
                        joining,
                    ))
                }
                (ValidatorLifecycle::WaitingForReadiness(waiting), false, true) => {
                    tee_expired_pending.push(before.address());
                    ValidatorLifecycle::WaitingForReadiness(waiting)
                }
                (ValidatorLifecycle::Joining(joining), true, false) => {
                    if self.ocomp_registration(before.address())?.is_none() {
                        return Err(PrecompileError::Fatal(format!(
                            "certified active set contains validator {} without OCOMP admission",
                            before.address()
                        )));
                    }
                    ValidatorLifecycle::Active(state_machine::activate_at_boundary(joining))
                }
                (ValidatorLifecycle::Active(active), true, false) => {
                    ValidatorLifecycle::Active(state_machine::retain_active_at_boundary(active))
                }
                (ValidatorLifecycle::Active(_), false, false) => {
                    return Err(PrecompileError::Fatal(format!(
                        "validated boundary omitted active validator {}",
                        before.address()
                    )));
                }
                (ValidatorLifecycle::Exiting(exiting), true, false) => {
                    let changed_at = changed_at.ok_or_else(|| {
                        PrecompileError::Fatal(format!(
                            "exiting validator {} has no deactivation height",
                            before.address()
                        ))
                    })?;
                    if changed_at <= freeze_height {
                        return Err(PrecompileError::Fatal(format!(
                            "validated boundary retained validator {} that exited at {changed_at} before freeze {freeze_height}",
                            before.address()
                        )));
                    }
                    ValidatorLifecycle::Exiting(exiting)
                }
                (ValidatorLifecycle::Exiting(exiting), false, false) => {
                    let changed_at = changed_at.ok_or_else(|| {
                        PrecompileError::Fatal(format!(
                            "exiting validator {} has no deactivation height",
                            before.address()
                        ))
                    })?;
                    if changed_at > freeze_height {
                        return Err(PrecompileError::Fatal(format!(
                            "validated boundary omitted validator {} that exited at {changed_at} after freeze {freeze_height}",
                            before.address()
                        )));
                    }
                    transitioned_to_unbonding.push(before.address());
                    ValidatorLifecycle::Unbonding(state_machine::exclude_exiting_at_boundary(
                        exiting,
                    ))
                }
                (ValidatorLifecycle::JailRetained(jailed), true, false) => {
                    let jailed_at = before.stored_jailed_at();
                    if jailed_at <= freeze_height {
                        return Err(PrecompileError::Fatal(format!(
                            "validated boundary retained validator {} jailed at {jailed_at} before freeze {freeze_height}",
                            before.address()
                        )));
                    }
                    ValidatorLifecycle::JailRetained(jailed)
                }
                (ValidatorLifecycle::JailRetained(jailed), false, false) => {
                    let jailed_at = before.stored_jailed_at();
                    if jailed_at > freeze_height {
                        return Err(PrecompileError::Fatal(format!(
                            "validated boundary omitted validator {} jailed at {jailed_at} after freeze {freeze_height}",
                            before.address()
                        )));
                    }
                    ValidatorLifecycle::Jail(state_machine::exclude_jailed_at_boundary(jailed))
                }
                (ValidatorLifecycle::Joining(joining), false, false) => {
                    ValidatorLifecycle::Joining(joining)
                }
                (lifecycle, false, false) => lifecycle,
                (lifecycle, true, false) => {
                    return Err(PrecompileError::Fatal(format!(
                        "validated boundary included ineligible validator {} with status {}",
                        before.address(),
                        registered_status(&lifecycle)?
                    )));
                }
                (lifecycle, _, true) => {
                    return Err(PrecompileError::Fatal(format!(
                        "TEE expiry exclusion contains validator {} with ineligible status {}",
                        before.address(),
                        registered_status(&lifecycle)?
                    )));
                }
            };
            let after = before.clone().with_lifecycle(lifecycle)?;
            transitions.push((before, after));
        }

        let planned_participants: Vec<_> = transitions
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

        let pending = transitions.iter().any(|(_, after)| {
            matches!(
                after.lifecycle(),
                ValidatorLifecycle::WaitingForReadiness(_)
                    | ValidatorLifecycle::Joining(_)
                    | ValidatorLifecycle::Exiting(_)
                    | ValidatorLifecycle::JailRetained(_)
            )
        });

        // The planner above performs every fallible semantic check before this
        // checkpoint. Storage writes, hash, repair flag, and event commit as one
        // bundle even for direct legacy calls.
        let guard = self.storage.checkpoint_guard();
        for (before, after) in &transitions {
            self.persist_validator_state_delta(before, after)?;
        }
        self.active_consensus_set_hash.write(active_set_hash)?;
        self.pending_set_change.write(pending)?;
        self.emit(IValidatorSet::ConsensusSetUpdated {
            activeCount: active_count,
        })?;
        guard.commit();

        crate::metrics::record_reshared_set_activated(
            active_count,
            transitioned_to_unbonding.len(),
        );
        crate::metrics::record_pending_set_change(pending);
        for (_, after) in &transitions {
            if let Some(stored_status) = after.stored_status() {
                crate::metrics::record_validator_status(after.address(), stored_status);
            }
        }
        for addr in &tee_expired_active {
            crate::metrics::record_validator_status(*addr, status::PENDING);
            crate::metrics::record_validator_tee_expiry(*addr, "active_demoted");
        }
        for addr in &tee_expired_pending {
            crate::metrics::record_validator_tee_expiry(*addr, "pending_cleared");
        }
        crate::metrics::record_tee_expiry_exclusions(
            tee_expired_active.len(),
            tee_expired_pending.len(),
        );

        let block_number = self.storage.block_number().unwrap_or(0);
        journal_record(JournalRecord::ResharedSetActivated {
            wall_clock: iso8601_now(),
            block_number,
            active_count,
            transitioned_to_unbonding: transitioned_to_unbonding.len() as u64,
            pending_set_change: pending,
            active_set_hash: format!("{active_set_hash:?}"),
        });
        for addr in &transitioned_to_unbonding {
            journal_record(JournalRecord::ValidatorUnbonding {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{addr:?}"),
            });
        }

        let mut active = 0usize;
        let mut exiting = 0usize;
        let mut unbonding = 0usize;
        for (_, after) in &transitions {
            match after.lifecycle() {
                ValidatorLifecycle::Active(_) => active += 1,
                ValidatorLifecycle::Exiting(_) => exiting += 1,
                ValidatorLifecycle::Unbonding(_) => unbonding += 1,
                _ => {}
            }
        }
        crate::metrics::record_aggregate_status_counts(active, exiting, unbonding);

        info!(
            target: "outbe::validatorset",
            event = "reshared_set_activated",
            active_count,
            transitioned_to_unbonding = transitioned_to_unbonding.len(),
            pending_set_change = pending,
            block_number,
            active_set_hash = %active_set_hash,
            "DKG reshare activated; new active set committed",
        );
        for addr in &transitioned_to_unbonding {
            info!(
                target: "outbe::validatorset",
                event = "validator_unbonding",
                validator = %addr,
                block_number,
                "validator transitioned EXITING -> UNBONDING (excluded from new set)",
            );
        }
        for addr in &tee_expired_active {
            warn!(
                target: "outbe::validatorset",
                event = "validator_tee_expired_demoted",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "certified freeze-height TEE expiry demoted ACTIVE validator to PENDING"
            );
        }
        for addr in &tee_expired_pending {
            warn!(
                target: "outbe::validatorset",
                event = "validator_tee_expired_readiness_cleared",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "certified freeze-height TEE expiry cleared PENDING validator readiness"
            );
        }

        Ok(())
    }
}
