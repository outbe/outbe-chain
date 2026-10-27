mod adapter;
mod state;

#[cfg(test)]
mod tests;

pub(crate) use adapter::StoredValidatorFields;
pub use state::{
    Active, ConsensusPubkey, Exiting, HistoryCounters, Inactive, Jail, JailRetained, Joining,
    P2pInfo, StakeProjection, Unbonding, ValidatorHistory, ValidatorLifecycle, ValidatorState,
    WaitingForReadiness,
};

use std::num::NonZeroU64;

use alloy_primitives::U256;
use outbe_primitives::error::{PrecompileError, Result};

use crate::errors::ValidatorSetError;

/// Creates the first registered lifecycle bundle for an absent address.
pub(crate) fn register(
    state: ValidatorState,
    registry_index: NonZeroU64,
    (consensus_pubkey, p2p): (ConsensusPubkey, P2pInfo),
    (stake, minimum): (StakeProjection, U256),
    joined_at_height: u64,
) -> Result<ValidatorState> {
    if !matches!(state.lifecycle, ValidatorLifecycle::Absent) {
        return Err(ValidatorSetError::ValidatorIsAlreadyRegistered.into());
    }
    if consensus_pubkey == [0; 48] {
        return Err(ValidatorSetError::ZeroConsensusPublicKey.into());
    }
    require_at_least_minimum(stake, minimum)?;
    state.with_lifecycle(ValidatorLifecycle::WaitingForReadiness(
        WaitingForReadiness {
            registry_index,
            consensus_pubkey,
            p2p,
            stake,
            history: ValidatorHistory::fresh(joined_at_height),
        },
    ))
}

/// Re-registers an inactive record in-place, retaining its dense index while
/// replacing identity material and clearing lifecycle-owned residue.
pub fn reregister(
    state: Inactive,
    (consensus_pubkey, p2p): (ConsensusPubkey, P2pInfo),
    (stake, minimum): (StakeProjection, U256),
    joined_at_height: u64,
) -> Result<WaitingForReadiness> {
    if consensus_pubkey == [0; 48] {
        return Err(ValidatorSetError::ZeroConsensusPublicKey.into());
    }
    require_at_least_minimum(stake, minimum)?;
    Ok(WaitingForReadiness {
        registry_index: state.registry_index,
        consensus_pubkey,
        p2p,
        stake,
        history: ValidatorHistory::fresh(joined_at_height),
    })
}

/// Records the validator operator's readiness confirmation.
pub fn confirm_ready(state: WaitingForReadiness) -> Joining {
    Joining {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history,
    }
}

fn deactivate_history(history: ValidatorHistory, height: u64) -> Result<ValidatorHistory> {
    if height == 0 {
        return Err(ValidatorSetError::ValidatorDeactivationHeightZero.into());
    }
    Ok(history.with_last_deactivated_at_height(Some(height)))
}

/// Full voluntary exit from `WaitingForReadiness` (`bonded == 0`).
pub fn unbond_waiting_for_readiness(
    state: WaitingForReadiness,
    stake: StakeProjection,
    deactivated_at_height: u64,
) -> Result<Unbonding> {
    require_fully_unstaked(stake)?;
    let history = deactivate_history(state.history, deactivated_at_height)?;
    Ok(Unbonding {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake,
        history,
    })
}

/// Full voluntary exit from `Joining` (`bonded == 0`).
pub fn unbond_joining(
    state: Joining,
    stake: StakeProjection,
    deactivated_at_height: u64,
) -> Result<Unbonding> {
    unbond_waiting_for_readiness(expire_joining_tee(state), stake, deactivated_at_height)
}

/// Demotes a ready joiner after a slash drops its bonded stake below the
/// minimum and consumes its readiness confirmation.
///
/// `deactivated_at_height` is the block that dropped the bond below the
/// minimum. The boundary planner uses it to keep a joiner who was already
/// frozen into the reshare target accountable for that epoch.
pub fn demote_joining(
    state: Joining,
    stake: StakeProjection,
    minimum: U256,
    deactivated_at_height: u64,
) -> Result<WaitingForReadiness> {
    require_below_minimum(stake, minimum)?;
    let history = deactivate_history(state.history, deactivated_at_height)?;
    Ok(WaitingForReadiness {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake,
        history,
    })
}

/// Keeps a joiner who lost eligibility after the reshare freeze in the
/// certified committee until the next boundary can drop them.
fn exit_demoted_joiner(joiner: JoinerPayload, deactivated_at_height: u64) -> Result<Exiting> {
    require_canonical_stake(joiner.stake)?;
    let history = deactivate_history(joiner.history, deactivated_at_height)?;
    Ok(Exiting {
        registry_index: joiner.registry_index,
        consensus_pubkey: joiner.consensus_pubkey,
        p2p: joiner.p2p,
        stake: joiner.stake,
        history,
    })
}

/// The payload of a frozen joiner that lost eligibility after the freeze.
struct JoinerPayload {
    registry_index: NonZeroU64,
    consensus_pubkey: ConsensusPubkey,
    p2p: P2pInfo,
    stake: StakeProjection,
    history: ValidatorHistory,
}

/// Boundary retain for a frozen joiner now persisted as `Unbonding`.
pub fn exit_unbonding_at_boundary(
    state: Unbonding,
    deactivated_at_height: u64,
) -> Result<Exiting> {
    exit_demoted_joiner(
        JoinerPayload {
            registry_index: state.registry_index,
            consensus_pubkey: state.consensus_pubkey,
            p2p: state.p2p,
            stake: state.stake,
            history: state.history,
        },
        deactivated_at_height,
    )
}

/// Boundary retain for a frozen joiner now persisted as `Inactive`.
pub fn exit_inactive_at_boundary(
    state: Inactive,
    deactivated_at_height: u64,
) -> Result<Exiting> {
    exit_demoted_joiner(
        JoinerPayload {
            registry_index: state.registry_index,
            consensus_pubkey: state.consensus_pubkey,
            p2p: state.p2p,
            stake: StakeProjection::zero(),
            history: state.history,
        },
        deactivated_at_height,
    )
}

/// Boundary retain for a frozen joiner who remained in `WaitingForReadiness`
/// without confirming readiness again.
pub fn exit_waiting_for_readiness_at_boundary(
    state: WaitingForReadiness,
    deactivated_at_height: u64,
) -> Result<Exiting> {
    exit_demoted_joiner(
        JoinerPayload {
            registry_index: state.registry_index,
            consensus_pubkey: state.consensus_pubkey,
            p2p: state.p2p,
            stake: state.stake,
            history: state.history,
        },
        deactivated_at_height,
    )
}

/// Activates an included, readiness-confirmed joiner at a validated boundary.
pub fn activate_at_boundary(state: Joining) -> Active {
    Active {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history.with_last_deactivated_at_height(None),
    }
}

/// Retains an existing active validator across a valid same-member boundary.
pub fn retain_active_at_boundary(state: Active) -> Active {
    state
}

/// Invalidates an active validator's readiness after its TEE lease expired.
/// The validator keeps its identity and stake, but must confirm readiness again
/// before it can join a later DKG target.
pub fn expire_active_tee(state: Active) -> WaitingForReadiness {
    WaitingForReadiness {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history,
    }
}

/// Consumes a joiner's readiness confirmation after its TEE lease expired.
pub fn expire_joining_tee(state: Joining) -> WaitingForReadiness {
    WaitingForReadiness {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history,
    }
}

/// Starts voluntary or forced exit while retaining current committee
/// accountability until the next validated boundary.
pub fn begin_exit(
    state: Active,
    stake: StakeProjection,
    deactivated_at_height: u64,
) -> Result<Exiting> {
    exit_demoted_joiner(
        JoinerPayload {
            registry_index: state.registry_index,
            consensus_pubkey: state.consensus_pubkey,
            p2p: state.p2p,
            stake,
            history: state.history,
        },
        deactivated_at_height,
    )
}

/// Jails an active validator without clearing the old committee share.
pub fn jail(state: Active, jailed_at: u64) -> Result<JailRetained> {
    if jailed_at == 0 {
        return Err(ValidatorSetError::ValidatorJailHeightZero.into());
    }
    Ok(JailRetained {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state
            .history
            .with_last_deactivated_at_height(Some(jailed_at)),
        jailed_at,
    })
}

/// Clears a jailed validator's old committee accountability and current miss
/// slate at a validated exclusion boundary. The full history shape remains so
/// late finalized-parent accounting can record historical misses afterward.
pub fn exclude_jailed_at_boundary(state: JailRetained) -> Jail {
    Jail {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history.with_missed_counters_cleared(),
        jailed_at: state.jailed_at,
    }
}

/// A retained jailed validator may request permanent exit only by fully
/// unstaking. It remains accountable as `Exiting` until the boundary.
pub fn full_exit_jailed_retained(state: JailRetained, stake: StakeProjection) -> Result<Exiting> {
    require_fully_unstaked(stake)?;
    Ok(Exiting {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake,
        // Preserve the jail height: that is when future-committee eligibility
        // changed. A later unstake must not make a stale boundary include it.
        history: state.history,
    })
}

/// Returns an already-excluded jailed validator to the unconfirmed readiness
/// state after the cooldown and minimum-stake checks. This function resets the
/// missed counters again because historical finalized-parent accounting may
/// have populated them after the exclusion boundary.
pub fn unjail(
    state: Jail,
    current_height: u64,
    cooldown_blocks: u64,
    minimum: U256,
) -> Result<WaitingForReadiness> {
    require_at_least_minimum(state.stake, minimum)?;
    let ready_at = state
        .jailed_at
        .checked_add(cooldown_blocks)
        .ok_or(ValidatorSetError::UnjailCooldownHeightOverflow)?;
    if current_height < ready_at {
        return Err(ValidatorSetError::UnjailCooldownNotElapsed {
            ready_at,
            current_height,
        }
        .into());
    }
    Ok(WaitingForReadiness {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history.with_missed_counters_cleared(),
    })
}

/// An already-excluded jailed validator does not need a second committee
/// boundary. Full unstake therefore enters `Unbonding` directly instead of
/// introducing an `EXITING` state without a share.
pub fn full_exit_jailed(state: Jail, stake: StakeProjection) -> Result<Unbonding> {
    require_fully_unstaked(stake)?;
    Ok(Unbonding {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake,
        history: state.history,
    })
}

/// Clears an exiting validator's live share at a validated boundary.
pub fn exclude_exiting_at_boundary(state: Exiting) -> Unbonding {
    Unbonding {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        stake: state.stake,
        history: state.history,
    }
}

/// Completes economic exit after Staking has proved zero bonded value and no
/// remaining live claim, represented here by a cleared compatibility hint.
pub fn complete_unbonding(state: Unbonding) -> Result<Inactive> {
    if !state.stake.bonded.is_zero() || state.stake.unbonding_end_hint.is_some() {
        return Err(ValidatorSetError::CannotCompleteUnbondingWithRemainingStakeOrClaim.into());
    }
    Ok(Inactive {
        registry_index: state.registry_index,
        consensus_pubkey: state.consensus_pubkey,
        p2p: state.p2p,
        history: state.history,
    })
}

/// Consumes an inactive registry entry after storage cleanup has repaired the
/// dense and reverse indexes.
pub fn cleanup(_state: Inactive) -> ValidatorLifecycle {
    ValidatorLifecycle::Absent
}

/// Replaces the ValidatorSet stake mirror without changing lifecycle meaning.
/// Threshold-crossing callers must use the specific transition functions above.
pub(crate) fn with_stake(
    mut lifecycle: ValidatorLifecycle,
    stake: StakeProjection,
) -> Result<ValidatorLifecycle> {
    require_canonical_stake(stake)?;
    if let Some(rejection) = stake_target_rejection(&lifecycle) {
        return Err(rejection);
    }
    match &mut lifecycle {
        ValidatorLifecycle::WaitingForReadiness(state) => state.stake = stake,
        ValidatorLifecycle::Joining(state) => state.stake = stake,
        ValidatorLifecycle::Active(state) => state.stake = stake,
        ValidatorLifecycle::JailRetained(state) => state.stake = stake,
        ValidatorLifecycle::Jail(state) => state.stake = stake,
        ValidatorLifecycle::Exiting(state) => state.stake = stake,
        ValidatorLifecycle::Unbonding(state) => state.stake = stake,
        ValidatorLifecycle::Absent | ValidatorLifecycle::Inactive(_) => {}
    }
    Ok(lifecycle)
}

/// The rejection of a stake update for a lifecycle that cannot hold stake:
/// an absent address or an inactive registry entry.
pub(crate) fn stake_target_rejection(lifecycle: &ValidatorLifecycle) -> Option<PrecompileError> {
    match lifecycle {
        ValidatorLifecycle::Absent => Some(ValidatorSetError::ValidatorNotRegistered.into()),
        ValidatorLifecycle::Inactive(_) => {
            Some(ValidatorSetError::InactiveValidatorMustReregisterBeforeStaking.into())
        }
        _ => None,
    }
}

/// Replaces the stake mirror of an UNBONDING validator.
pub(crate) fn with_unbonding_stake(state: Unbonding, stake: StakeProjection) -> Result<Unbonding> {
    require_canonical_stake(stake)?;
    Ok(Unbonding { stake, ..state })
}

/// Updates informational P2P data without changing lifecycle eligibility.
pub(crate) fn with_p2p(
    mut lifecycle: ValidatorLifecycle,
    p2p: P2pInfo,
) -> Result<ValidatorLifecycle> {
    match &mut lifecycle {
        ValidatorLifecycle::WaitingForReadiness(state) => state.p2p = p2p,
        ValidatorLifecycle::Joining(state) => state.p2p = p2p,
        ValidatorLifecycle::Active(state) => state.p2p = p2p,
        ValidatorLifecycle::JailRetained(state) => state.p2p = p2p,
        ValidatorLifecycle::Jail(state) => state.p2p = p2p,
        ValidatorLifecycle::Exiting(state) => state.p2p = p2p,
        ValidatorLifecycle::Unbonding(state) => state.p2p = p2p,
        ValidatorLifecycle::Inactive(state) => state.p2p = p2p,
        ValidatorLifecycle::Absent => {
            return Err(ValidatorSetError::ValidatorNotRegistered.into());
        }
    }
    Ok(lifecycle)
}

/// Updates retained history without permitting a lifecycle rewrite.
pub(crate) fn with_history(
    mut lifecycle: ValidatorLifecycle,
    history: ValidatorHistory,
) -> Result<ValidatorLifecycle> {
    if history.last_deactivated_at_height == Some(0) {
        return Err(ValidatorSetError::DeactivationHeightZeroSentinel.into());
    }
    if let Some(rejection) = history_rejection(&lifecycle, &history) {
        return Err(rejection);
    }
    if let Some(retained) = history_mut(&mut lifecycle) {
        *retained = history;
    }
    Ok(lifecycle)
}

/// The rejection of a history that contradicts the lifecycle variant.
fn history_rejection(
    lifecycle: &ValidatorLifecycle,
    history: &ValidatorHistory,
) -> Option<PrecompileError> {
    let deactivated_at = history.last_deactivated_at_height;
    let error = match lifecycle {
        ValidatorLifecycle::Absent => ValidatorSetError::ValidatorNotRegistered,
        ValidatorLifecycle::Active(_) if deactivated_at.is_some() => {
            ValidatorSetError::ActiveHistoryHasDeactivationHeight
        }
        ValidatorLifecycle::JailRetained(state) if deactivated_at != Some(state.jailed_at) => {
            ValidatorSetError::RetainedJailHistoryHeightMismatch
        }
        ValidatorLifecycle::Jail(state) if deactivated_at != Some(state.jailed_at) => {
            ValidatorSetError::JailHistoryHeightMismatch
        }
        ValidatorLifecycle::Exiting(_) if deactivated_at.is_none() => {
            ValidatorSetError::ExitingHistoryMissingDeactivationHeight
        }
        ValidatorLifecycle::Unbonding(_) if deactivated_at.is_none() => {
            ValidatorSetError::UnbondingHistoryMissingDeactivationHeight
        }
        ValidatorLifecycle::Inactive(_) if deactivated_at.is_none() => {
            ValidatorSetError::InactiveHistoryMissingDeactivationHeight
        }
        _ => return None,
    };
    Some(error.into())
}

fn history_mut(lifecycle: &mut ValidatorLifecycle) -> Option<&mut ValidatorHistory> {
    match lifecycle {
        ValidatorLifecycle::Absent => None,
        ValidatorLifecycle::WaitingForReadiness(state) => Some(&mut state.history),
        ValidatorLifecycle::Joining(state) => Some(&mut state.history),
        ValidatorLifecycle::Active(state) => Some(&mut state.history),
        ValidatorLifecycle::JailRetained(state) => Some(&mut state.history),
        ValidatorLifecycle::Jail(state) => Some(&mut state.history),
        ValidatorLifecycle::Exiting(state) => Some(&mut state.history),
        ValidatorLifecycle::Unbonding(state) => Some(&mut state.history),
        ValidatorLifecycle::Inactive(state) => Some(&mut state.history),
    }
}

fn require_at_least_minimum(stake: StakeProjection, minimum: U256) -> Result<()> {
    require_canonical_stake(stake)?;
    if stake.bonded < minimum {
        return Err(ValidatorSetError::BondedStakeBelowMinimum {
            bonded: stake.bonded,
            minimum,
        }
        .into());
    }
    Ok(())
}

fn require_below_minimum(stake: StakeProjection, minimum: U256) -> Result<()> {
    require_canonical_stake(stake)?;
    if stake.bonded >= minimum {
        return Err(ValidatorSetError::BondedStakeNotBelowMinimum {
            bonded: stake.bonded,
            minimum,
        }
        .into());
    }
    Ok(())
}

fn require_fully_unstaked(stake: StakeProjection) -> Result<()> {
    require_canonical_stake(stake)?;
    if !stake.bonded.is_zero() {
        return Err(ValidatorSetError::JailedValidatorMustFullyUnstakeForExit.into());
    }
    Ok(())
}

fn require_canonical_stake(stake: StakeProjection) -> Result<()> {
    if stake.unbonding_end_hint == Some(0) {
        return Err(ValidatorSetError::UnbondingEndHintZeroSentinel.into());
    }
    Ok(())
}
