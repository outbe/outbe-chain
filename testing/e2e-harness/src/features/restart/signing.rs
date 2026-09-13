use super::committee_checkpoint_json;
use super::pending_boundary_at;
use super::restart_assert_live;
use super::restart_fresh_checkpoint;
use super::restart_ports;
use super::restart_public_state;
use super::restart_remaining_tries;
use super::restart_require_membership;
use super::restart_snapshot;
use super::restart_validate_frozen_target;
use super::RestartFrozenRound;
use super::RestartFrozenTarget;

use crate::world::rpc::FinalizedCheckpoint;

use crate::world::World;

use eyre::ensure;
use eyre::eyre;
use eyre::Result;
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;

use serde_json::json;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

// A rotation invalidates a counter comparison, not the recovery. The caller may
// choose a new fully eligible window within the ORIGINAL remaining allowance.
pub(super) fn restart_signing_comparison(
    before_height: u64,
    after_height: u64,
    before_epoch: u64,
    after_epoch: u64,
    before_misses: u64,
    after_misses: u64,
) -> Result<bool> {
    let target = before_height
        .checked_add(5)
        .and_then(|h| h.checked_add(LATE_FINALIZE_WINDOW_K))
        .ok_or_else(|| eyre!("signing target overflow"))?;
    ensure!(
        after_height >= target && after_epoch >= before_epoch,
        "incomplete or regressed signing window"
    );
    if after_epoch != before_epoch {
        return Ok(false);
    }
    ensure!(
        after_misses == before_misses,
        "recovered eligible validator accumulated voter misses"
    );
    Ok(true)
}

pub(super) fn restart_prove_signing(
    world: &mut World,
    address: &str,
    drain_tries: u32,
    window_tries: u32,
) -> Result<FinalizedCheckpoint> {
    restart_assert_live(world)?;
    let ports = restart_ports(world)?;
    let drain_deadline =
        std::time::Instant::now() + Duration::from_secs(u64::from(drain_tries) * 3);
    let original = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing restart checkpoint"))?;
    world
        .rpc
        .wait_finalized_checkpoint(&ports, original.height, drain_tries)?;
    let fresh = world.rpc.fresh_finality_target(&ports)?;
    let drain = fresh
        .checked_add(LATE_FINALIZE_WINDOW_K)
        .ok_or_else(|| eyre!("eligibility target overflow"))?;
    let mut before = world.rpc.wait_finalized_checkpoint(
        &ports,
        drain,
        restart_remaining_tries(drain_deadline)?,
    )?;
    let mut baseline = restart_snapshot(world, before, address, "restart_signing_baseline")?;
    restart_require_membership(world, &baseline, 2)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(window_tries) * 3);
    loop {
        restart_assert_live(world)?;
        ensure!(
            std::time::Instant::now() < deadline,
            "fully closed eligible signing window did not finalize"
        );
        let target = before
            .height
            .checked_add(5)
            .and_then(|h| h.checked_add(LATE_FINALIZE_WINDOW_K))
            .ok_or_else(|| eyre!("signing target overflow"))?;
        let observed = ports
            .iter()
            .map(|&port| world.rpc.finalized_result(port))
            .collect::<Result<Vec<_>>>()?;
        if observed.iter().any(|&height| height < target) {
            sleep(Duration::from_secs(3));
            continue;
        }
        let after = world.rpc.wait_finalized_checkpoint(&ports, target, 1)?;
        let state = restart_snapshot(world, after, address, "restart_signing_closed")?;
        restart_require_membership(world, &state, 2)?;
        if restart_signing_comparison(
            before.height,
            after.height,
            baseline.epoch,
            state.epoch,
            baseline.voter_misses,
            state.voter_misses,
        )? {
            restart_assert_live(world)?;
            return Ok(after);
        }
        // A new boundary can itself have delayed pre-eligibility accounting.
        // Drain it before choosing another window, without resetting the deadline.
        let drained = after
            .height
            .checked_add(LATE_FINALIZE_WINDOW_K)
            .ok_or_else(|| eyre!("rotation drain overflow"))?;
        before = world.rpc.wait_finalized_checkpoint(
            &ports,
            drained,
            restart_remaining_tries(deadline)?,
        )?;
        baseline = restart_snapshot(world, before, address, "restart_signing_rotation_drain")?;
        restart_require_membership(world, &baseline, 2)?;
    }
}

pub(super) fn restart_boundary_transition(
    previous_epoch: u64,
    next_epoch: u64,
    boundary_epoch: Option<u64>,
    was_active: bool,
    now_active: bool,
    frozen: bool,
) -> Result<bool> {
    ensure!(!was_active || now_active, "restart admission was reversed");
    if let Some(epoch) = boundary_epoch {
        ensure!(
            previous_epoch.checked_add(1) == Some(epoch) && next_epoch == epoch,
            "duplicate, skipped, or mismatched canonical boundary epoch"
        );
        ensure!(
            was_active || now_active || !frozen,
            "frozen target boundary omitted the joiner"
        );
    } else {
        ensure!(
            next_epoch == previous_epoch && was_active == now_active,
            "partial activation or epoch transition without a canonical boundary"
        );
    }
    Ok(!was_active && now_active)
}

pub(super) fn restart_activation(world: &mut World, frozen: bool) -> Result<FinalizedCheckpoint> {
    let target: Option<RestartFrozenTarget> = if frozen {
        let row = world
            .state
            .restart_observations
            .iter()
            .find(|row| row["phase"] == "restart_frozen_target")
            .ok_or_else(|| eyre!("missing interrupted frozen commitment"))?;
        Some(serde_json::from_value(row["target"].clone())?)
    } else {
        None
    };
    let address = world
        .state
        .joiner_addr
        .clone()
        .ok_or_else(|| eyre!("missing joining identity"))?;
    ensure!(
        world
            .rpc
            .wait_participant(world.validators.primary_port(), &address, 90)?,
        "joining validator did not activate after restart"
    );
    let after = restart_fresh_checkpoint(world, 90)?;
    let before = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing pre-restart checkpoint"))?;
    let old_epoch = restart_public_state(
        world,
        world.validators.primary_port(),
        before.height,
        &address,
    )?
    .epoch;
    let mut previous_epoch = old_epoch;
    let mut activated = None;
    for height in before
        .height
        .checked_add(1)
        .ok_or_else(|| eyre!("boundary scan overflow"))?..=after.height
    {
        let artifact = pending_boundary_at(&world.rpc, world.validators.primary_port(), height)?;
        let state = restart_public_state(world, world.validators.primary_port(), height, &address)?;
        let now_active = state.status == 2;
        restart_require_membership(world, &state, if now_active { 2 } else { 1 })?;
        let admitted = restart_boundary_transition(
            previous_epoch,
            state.epoch,
            artifact.as_ref().map(|a| a.epoch),
            activated.is_some(),
            now_active,
            frozen,
        )?;
        if let Some(artifact) = artifact {
            let checkpoint = world
                .rpc
                .checkpoint_at(world.validators.primary_port(), height)?;
            restart_snapshot(world, checkpoint, &address, "restart_canonical_boundary")?;
            world.state.restart_observations.push(json!({
                "phase": "restart_boundary", "checkpoint": committee_checkpoint_json(checkpoint),
                "epoch": artifact.epoch, "dkg_cycle": artifact.dkg_cycle, "admitted": admitted,
                "freeze_height": artifact.freeze_height, "planned_activation_height": artifact.planned_activation_height,
                "target_set_hash": artifact.target_set_hash, "new_active_set": artifact.reshare.new_active_set,
                "outcome_hash": alloy_primitives::keccak256(&artifact.outcome),
            }));
            if let Some(target) = &target {
                ensure!(
                    artifact.dkg_cycle != target.round.cycle || admitted,
                    "interrupted frozen target activated more than once or without admission"
                );
                if admitted {
                    ensure!(
                        artifact.is_validator_set_change && !artifact.is_full_dkg,
                        "interrupted reshare was replaced by a different ceremony mode"
                    );
                    restart_validate_frozen_target(
                        target,
                        &RestartFrozenRound {
                            cycle: artifact.dkg_cycle,
                            freeze: artifact.freeze_height,
                            planned: artifact.planned_activation_height,
                        },
                        artifact.target_set_hash,
                        &artifact.reshare.new_active_set,
                        height,
                    )?;
                }
            }
            if admitted {
                activated = Some(checkpoint);
            }
        }
        previous_epoch = state.epoch;
    }
    let state = restart_snapshot(world, after, &address, "restart_activated_terminal")?;
    restart_require_membership(world, &state, 2)?;
    ensure!(
        state.epoch == previous_epoch,
        "terminal epoch differs from canonical boundary history"
    );
    activated.ok_or_else(|| eyre!("restart recovery has no canonical admission boundary"))
}

pub(super) fn wait_for_dkg_retry_snapshot(
    keys_dir: impl AsRef<Path>,
    validator: usize,
    file: &str,
) {
    let snapshot = keys_dir.as_ref().join(file);
    for _ in 0..1_800 {
        if snapshot.exists() {
            return;
        }
        sleep(Duration::from_millis(100));
    }
    panic!("validator-{validator} did not persist {file} before the restart crash point");
}
