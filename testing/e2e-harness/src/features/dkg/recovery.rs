//! Recovery of a stalled frozen DKG target and its canonical activation.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256};
use eyre::{ensure, eyre, Result};
use serde_json::json;

use super::frozen::{dkg_log, log_field, retained_frozen_target, FrozenTarget};
use super::{
    dkg_addresses, dkg_boundary_at, dkg_epoch_at, dkg_membership_at, dkg_ports,
    record_dkg_checkpoint, BoundaryWitness, DkgMembership,
};
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::World;

pub(super) fn dkg_activation_anchor(boundary_commit_height: u64) -> Result<u64> {
    // The first block of the new epoch carries the boundary artifact. That block
    // is one block after the activation anchor that the rotation schedule uses.
    boundary_commit_height
        .checked_sub(1)
        .ok_or_else(|| eyre!("DKG boundary commit height cannot be zero"))
}

pub(super) fn validate_dkg_activation(
    boundary: &BoundaryWitness,
    target: &FrozenTarget,
    members: &[Address],
    old_epoch: u64,
    require_delayed: bool,
) -> Result<()> {
    let anchor = dkg_activation_anchor(boundary.height)?;
    let mut actual = boundary.members.clone();
    let mut expected = members.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    ensure!(
        boundary.cycle == target.cycle
            && boundary.freeze == target.freeze
            && boundary.planned == target.planned
            && anchor >= target.planned
            && boundary.target_hash != B256::ZERO
            && boundary.epoch
                == old_epoch
                    .checked_add(1)
                    .ok_or_else(|| eyre!("DKG epoch overflow"))?
            && actual == expected,
        "activation does not match the frozen DKG target"
    );
    ensure!(
        !require_delayed || anchor > target.planned,
        "activation was not delayed beyond its planned anchor"
    );
    Ok(())
}

pub(super) fn validate_reveal_interval(
    log: &str,
    target: &FrozenTarget,
    boundary_commit_height: u64,
    expected: &str,
) -> Result<()> {
    let mut inside = false;
    let mut revealed = false;
    let cycle = target.cycle.to_string();
    let height = dkg_activation_anchor(boundary_commit_height)?.to_string();
    for line in log.lines() {
        if line.contains("freezing validator set and starting DKG rotation") {
            inside = log_field(line, "dkg_cycle") == Some(cycle.as_str());
            if inside {
                revealed = false;
            }
        }
        if inside
            && line.contains("DKG: a validator's individual share was REVEALED")
            && log_field(line, "revealed_validator") == Some(expected)
        {
            revealed = true;
        }
        if line.contains("VRF/DKG material activated")
            && log_field(line, "dkg_cycle") == Some(cycle.as_str())
        {
            ensure!(
                inside && revealed && log_field(line, "activation_height") == Some(height.as_str()),
                "offline reveal is not bound to the matching DKG activation interval"
            );
            return Ok(());
        }
    }
    Err(eyre!("missing matching reveal/activation interval"))
}

pub(super) fn recovered_checkpoint_attempts(remaining: Duration) -> Result<u32> {
    // The shared checkpoint helper waits three seconds after an unavailable
    // observation. Do not grant it a fresh budget after each recovery poll.
    let attempts = u32::try_from(remaining.as_secs() / 3)?;
    ensure!(
        attempts > 0,
        "no DKG recovery checkpoint readiness budget remains"
    );
    Ok(attempts)
}

struct RecoveryScan {
    target: FrozenTarget,
    members: Vec<Address>,
    old_epoch: u64,
    off_grid: bool,
    next: u64,
    activated: Option<u64>,
    fresh_target: Option<u64>,
}

impl RecoveryScan {
    fn scan_until(&mut self, world: &mut World, ports: &[u16], height: u64) -> Result<()> {
        while self.next <= height {
            if let Some(boundary) = dkg_boundary_at(world, ports, self.next)? {
                self.observe_boundary(world, ports, boundary)?;
            }
            if self.activated.is_none() {
                let at = world.rpc.checkpoint_at(ports[0], self.next)?;
                dkg_membership_at(
                    world,
                    ports,
                    at,
                    &DkgMembership {
                        members: &self.members[..4],
                        joiner: self.members[4],
                        status: 1,
                    },
                )?;
            }
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| eyre!("DKG scan height overflow"))?;
        }
        Ok(())
    }

    fn observe_boundary(
        &mut self,
        world: &mut World,
        ports: &[u16],
        boundary: BoundaryWitness,
    ) -> Result<()> {
        if self.activated.is_some() {
            ensure!(
                boundary.epoch
                    != self
                        .old_epoch
                        .checked_add(1)
                        .ok_or_else(|| eyre!("epoch overflow"))?,
                "recovered DKG epoch activated twice"
            );
        }
        if boundary.cycle != self.target.cycle {
            return Ok(());
        }
        ensure!(
            self.activated.is_none(),
            "frozen DKG target activated twice"
        );
        validate_dkg_activation(
            &boundary,
            &self.target,
            &self.members,
            self.old_epoch,
            self.off_grid,
        )?;
        let at = world.rpc.checkpoint_at(ports[0], self.next)?;
        dkg_membership_at(
            world,
            ports,
            at,
            &DkgMembership {
                members: &self.members,
                joiner: self.members[4],
                status: 2,
            },
        )?;
        world
            .state
            .restart_observations
            .push(json!({"phase": "dkg_activation", "boundary": boundary}));
        self.activated = Some(self.next);
        self.fresh_target = Some(world.rpc.fresh_finality_target(ports)?);
        Ok(())
    }
}

fn finish_dkg_recovery(
    world: &mut World,
    scan: &RecoveryScan,
    ports: &[u16],
    point: FinalizedCheckpoint,
    require_reveal: bool,
) -> Result<()> {
    dkg_membership_at(
        world,
        ports,
        point,
        &DkgMembership {
            members: &scan.members,
            joiner: scan.members[4],
            status: 2,
        },
    )?;
    if require_reveal {
        let expected = world
            .state
            .expected_dkg_reveal
            .clone()
            .ok_or_else(|| eyre!("missing expected reveal identity"))?;
        validate_reveal_interval(
            &dkg_log(world, 0)?,
            &scan.target,
            scan.activated
                .ok_or_else(|| eyre!("missing activated boundary"))?,
            &expected,
        )?;
    }
    dkg_ports(world, &[0, 1, 2, 3])?;
    record_dkg_checkpoint(world, "dkg_recovered_finality", ports, point);
    Ok(())
}

pub(super) fn wait_recovered_dkg(
    world: &mut World,
    tries: u32,
    require_reveal: bool,
    off_grid: bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(u64::from(tries) * 10);
    let target = retained_frozen_target(world)?;
    let members = dkg_addresses(world, 5)?;
    let ports = dkg_ports(world, &[0, 1, 2, 3])?;
    let old_epoch = dkg_epoch_at(world, ports[0], target.freeze)?;
    let next = target
        .freeze
        .checked_add(1)
        .ok_or_else(|| eyre!("DKG scan height overflow"))?;
    let mut scan = RecoveryScan {
        target,
        members,
        old_epoch,
        off_grid,
        next,
        activated: None,
        fresh_target: None,
    };
    loop {
        let ports = dkg_ports(world, &[0, 1, 2, 3])?;
        let attempts =
            recovered_checkpoint_attempts(deadline.saturating_duration_since(Instant::now()))?;
        // A newly restored owned process may not serve finalized RPC yet.
        // Keep every expected peer and the helper's final per-port errors.
        // Canonical hash/root disagreement still fails immediately.
        let point = world.rpc.wait_finalized_checkpoint(&ports, 0, attempts)?;
        ensure!(
            Instant::now() < deadline,
            "DKG recovery checkpoint became ready after its observation deadline"
        );
        scan.scan_until(world, &ports, point.height)?;
        if scan
            .fresh_target
            .is_some_and(|height| point.height >= height)
        {
            return finish_dkg_recovery(world, &scan, &ports, point, require_reveal);
        }
        ensure!(
            Instant::now() < deadline,
            "frozen DKG target did not recover with fresh expected-peer finality"
        );
        sleep(Duration::from_secs(10));
    }
}
