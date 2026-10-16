//! The frozen DKG target each survivor logged and the harness retained.

use eyre::{ensure, eyre, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{dkg_addresses, dkg_membership_at, DkgMembership};
use crate::world::World;

pub(super) fn dkg_log(world: &mut World, index: usize) -> Result<String> {
    world
        .state
        .lifecycle_incarnations
        .get_mut(&index)
        .ok_or_else(|| eyre!("missing owned DKG log interval"))?
        .node_log
        .read()
}

pub(super) fn log_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field}=");
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FrozenTarget {
    pub cycle: u64,
    pub freeze: u64,
    pub planned: u64,
}

pub(super) fn frozen_target(log: &str, earliest_freeze: u64) -> Result<Option<FrozenTarget>> {
    let mut target = None;
    for line in log
        .lines()
        .filter(|line| line.contains("freezing validator set and starting DKG rotation"))
    {
        let number = |key| -> Result<u64> {
            log_field(line, key)
                .ok_or_else(|| eyre!("frozen-target record omitted {key}"))?
                .parse()
                .map_err(|_| eyre!("malformed frozen-target {key}"))
        };
        let current = FrozenTarget {
            cycle: number("dkg_cycle")?,
            freeze: number("freeze_height")?,
            planned: number("planned_activation_height")?,
        };
        ensure!(
            current.freeze <= current.planned,
            "invalid frozen-target schedule"
        );
        if current.freeze < earliest_freeze {
            continue;
        }
        if let Some(previous) = &target {
            ensure!(previous == &current, "frozen target was replaced");
        }
        target = Some(current);
    }
    Ok(target)
}

pub(super) fn observed_frozen_target(world: &mut World) -> Result<FrozenTarget> {
    let earliest = dkg_ready_height(world)?;
    let mut agreed = None;
    for index in 0..3 {
        let current = frozen_target(&dkg_log(world, index)?, earliest)?
            .ok_or_else(|| eyre!("validator-{index} has no current frozen target"))?;
        if let Some(expected) = &agreed {
            ensure!(expected == &current, "survivors froze different targets");
        }
        agreed = Some(current);
    }
    agreed.ok_or_else(|| eyre!("no frozen target observations"))
}

pub(super) fn dkg_ready_height(world: &World) -> Result<u64> {
    let ready = world
        .state
        .restart_observations
        .iter()
        .find(|row| row["phase"] == "dkg_ready")
        .ok_or_else(|| eyre!("missing ready receipt"))?;
    let encoded = ready["receipt"]["blockNumber"]
        .as_str()
        .ok_or_else(|| eyre!("ready receipt omitted height"))?;
    Ok(u64::from_str_radix(
        encoded
            .strip_prefix("0x")
            .ok_or_else(|| eyre!("invalid receipt height"))?,
        16,
    )?)
}

pub(super) fn retain_frozen_target(world: &mut World, ports: &[u16]) -> Result<FrozenTarget> {
    let target = observed_frozen_target(world)?;
    let point = world.rpc.checkpoint_at(ports[0], target.freeze)?;
    let members = dkg_addresses(world, 5)?;
    dkg_membership_at(
        world,
        ports,
        point,
        &DkgMembership {
            members: &members[..4],
            joiner: members[4],
            status: 1,
        },
    )?;
    world
        .state
        .restart_observations
        .push(json!({"phase": "dkg_frozen_target", "target": target}));
    Ok(target)
}

pub(super) fn retained_frozen_target(world: &World) -> Result<FrozenTarget> {
    let row = world
        .state
        .restart_observations
        .iter()
        .find(|row| row["phase"] == "dkg_frozen_target")
        .ok_or_else(|| eyre!("no retained frozen target"))?;
    Ok(serde_json::from_value(row["target"].clone())?)
}
