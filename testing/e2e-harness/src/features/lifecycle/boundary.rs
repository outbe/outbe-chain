use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{B256, U256};
use eyre::{ensure, eyre, Result};
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use serde_json::json;

use super::{
    account, address, checkpoint_json, exact_members, historical, live, members, observe, ports,
    retain, same_identity, Observation,
};
use crate::internal::eth;
use crate::world::World;

/// Find the membership-changing boundary, not the latest routine rotation.
/// Every candidate is tied to the exact shared finalized block and both sides
/// of the transition. The full public artifact remains in canonical block data.
pub(super) fn boundary(
    world: &mut World,
    included: bool,
    tries: u32,
    poll_seconds: u64,
) -> Result<(u64, Observation)> {
    let after = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing boundary anchor"))?;
    let expected_members = members(world, if included { 5 } else { 4 })?;
    let joiner = address(world, 4)?;
    let mut next = after
        .height
        .checked_add(1)
        .ok_or_else(|| eyre!("boundary height overflow"))?;
    for _ in 0..tries {
        live(world)?;
        let expected_ports = ports(world)?;
        let tip = world
            .rpc
            .wait_finalized_checkpoint(&expected_ports, after.height, 1)?;
        retain(
            world,
            "lifecycle_boundary_scan",
            json!({
                "checkpoint": checkpoint_json(tip), "expected_ports": expected_ports,
            }),
        );
        while next <= tip.height {
            let height = next;
            next = next
                .checked_add(1)
                .ok_or_else(|| eyre!("boundary scan overflow"))?;
            let block = eth::raw_json_result(
                &world.rpc.url(world.validators.primary_port()),
                "eth_getBlockByNumber",
                json!([format!("0x{height:x}"), false]),
            )?;
            let checkpoint = world
                .rpc
                .checkpoint_at(world.validators.primary_port(), height)?;
            let hash: B256 = serde_json::from_value(block["hash"].clone())?;
            ensure!(
                hash == checkpoint.block_hash,
                "boundary header identity mismatch"
            );
            let extra = block["extraData"]
                .as_str()
                .ok_or_else(|| eyre!("boundary header omitted extraData"))?;
            let bytes = hex::decode(
                extra
                    .strip_prefix("0x")
                    .ok_or_else(|| eyre!("invalid extraData prefix"))?,
            )?;
            let artifacts = decode_outbe_block_artifacts(&bytes)
                .map_err(|_| eyre!("invalid canonical block artifacts"))?;
            let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
                artifacts.consensus_header_artifact
            else {
                continue;
            };
            if artifact.reshare.new_active_set.contains(&joiner) != included {
                continue;
            }
            let before = historical(world, "lifecycle_boundary_previous", height - 1, 1)?;
            if account(&before)?.participant == included {
                continue; // A later rotation is not the admission/exit boundary.
            }
            let state = observe(world, "lifecycle_boundary", checkpoint)?;
            same_identity(account(&before)?, account(&state)?)?;
            exact_members(&artifact.reshare.new_active_set, &expected_members)?;
            exact_members(&state.consensus, &expected_members)?;
            ensure!(
                artifact.is_validator_set_change
                    && artifact.planned_activation_height <= height
                    && state.epoch == U256::from(artifact.epoch)
                    && state.epoch
                        == before
                            .epoch
                            .checked_add(U256::ONE)
                            .ok_or_else(|| eyre!("epoch overflow"))?
                    && account(&state)?.participant == included
                    && account(&state)?.status == if included { 2 } else { 4 },
                "canonical boundary does not prove lifecycle transition"
            );
            retain(
                world,
                "lifecycle_boundary_identity",
                json!({
                    "checkpoint": checkpoint_json(checkpoint), "epoch": artifact.epoch,
                    "dkg_cycle": artifact.dkg_cycle, "freeze_height": artifact.freeze_height,
                    "planned_activation_height": artifact.planned_activation_height,
                    "target_set_hash": artifact.target_set_hash,
                    "committee_set_hash": artifact.committee_set_hash,
                    "active_set_hash": artifact.reshare.active_set_hash,
                    "new_active_set": artifact.reshare.new_active_set,
                }),
            );
            return Ok((height, state));
        }
        sleep(Duration::from_secs(poll_seconds));
    }
    Err(eyre!("membership-changing DKG boundary did not finalize"))
}

pub(super) fn signing_heights(activation: u64) -> Result<(u64, u64)> {
    let closed = activation
        .checked_add(LATE_FINALIZE_WINDOW_K)
        .ok_or_else(|| eyre!("signing height overflow"))?;
    let end = closed
        .checked_add(5)
        .and_then(|height| height.checked_add(LATE_FINALIZE_WINDOW_K))
        .ok_or_else(|| eyre!("signing height overflow"))?;
    Ok((closed, end))
}

pub(super) fn signing_counts(activation: u64, closed: u64, end: u64) -> Result<()> {
    ensure!(
        closed >= activation && closed - activation <= 1,
        "misses exceed pre-eligibility allowance"
    );
    ensure!(
        end == closed,
        "eligible signing window accumulated voter misses"
    );
    Ok(())
}
