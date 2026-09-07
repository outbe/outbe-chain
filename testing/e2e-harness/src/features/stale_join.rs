//! Finalized readiness exclusion and admission on the same five owned nodes.

use std::thread::sleep;
use std::time::{Duration, Instant};

use cucumber::{then, when};
use eyre::{ensure, eyre, Result};
use serde_json::json;

use super::dkg::{
    capture_dkg_owner, dkg_addresses, dkg_boundary_at, dkg_epoch_at, dkg_membership_at, dkg_ports,
    record_dkg_checkpoint, BoundaryWitness,
};
use crate::internal::{addresses, eth};
use crate::world::rpc::TxOutcome;
use crate::world::World;

#[when("a staked joiner has not confirmed readiness")]
fn staked_joiner_unconfirmed(world: &mut World) {
    assert_eq!(
        world.validators.size(),
        4,
        "readiness fixture requires four founders"
    );
    let idx = world.validators.joiner_index();
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_caught_up_joiner(idx, &[])
        .expect("launch joiner");
    for index in 0..=idx {
        capture_dkg_owner(world, index).expect("capture readiness proof owner");
    }
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = eth::address_of(&key).expect("joiner public address");
    world.state.joiner_addr = Some(format!("{addr:#x}"));
    let amount = eth::coen(1000);
    let outcome: TxOutcome = eth::send_call_outcome(
        &world.rpc.url(world.validators.primary_port()),
        addresses::STK_ADDR,
        &key,
        &eth::IStaking::stakeCall {
            validatorAddress: addr,
            amount,
        },
        Some(amount),
    )
    .expect("stake without confirming readiness")
    .into();
    let ports = dkg_ports(world, &[0, 1, 2, 3, 4]).expect("all five readiness owners");
    let point = world
        .rpc
        .finalize_outcome(&outcome, &ports, 40)
        .expect("canonical finalized stake receipt");
    world.state.lifecycle_before = Some(point);
    world
        .state
        .restart_observations
        .push(json!({"phase": "readiness_stake_receipt",
        "transaction_hash": outcome.transaction_hash, "receipt": outcome.receipt}));
    let members = dkg_addresses(world, 5).expect("expected readiness identities");
    dkg_membership_at(world, &ports, point, &members[..4], addr, 1)
        .expect("finalized PENDING nonparticipant");
    for &port in &ports {
        let stake = eth::read_call_at_result(
            &world.rpc.url(port),
            addresses::STK_ADDR,
            &eth::IStaking::getStakeCall { validator: addr },
            point.height,
        )
        .expect("pinned pending stake");
        assert_eq!(
            stake, amount,
            "readiness fixture stake differs across peers"
        );
    }
    record_dkg_checkpoint(world, "readiness_stake_finalized", &ports, point);
}

/// Use the executed schedule, not a devnet absolute-height shortcut.
fn readiness_schedule(world: &World) -> Result<(u64, u64)> {
    let epoch = world.localnet.epoch_length_blocks()?;
    let genesis: serde_json::Value = serde_json::from_slice(&std::fs::read(
        world.localnet.scenario_dir().join("genesis.json"),
    )?)?;
    let prepare = genesis
        .pointer("/config/dkgPrepareWindowBlocks")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| eyre!("executed genesis omitted DKG prepare window"))?;
    ensure!(
        epoch > 0 && prepare > 0,
        "readiness proof requires a positive executed DKG schedule"
    );
    Ok((epoch, prepare.min(epoch)))
}

fn validate_readiness_boundary(boundary: &BoundaryWitness, epoch: u64, prepare: u64) -> Result<()> {
    ensure!(
        epoch > 0 && prepare > 0 && prepare <= epoch,
        "invalid executed DKG schedule"
    );
    ensure!(
        boundary.planned >= epoch
            && boundary.freeze == boundary.planned - prepare
            && boundary.height >= boundary.planned
            && boundary.target_hash != alloy_primitives::B256::ZERO,
        "boundary disagrees with the executed DKG schedule"
    );
    Ok(())
}

/// A target frozen before stake/confirmation cannot prove the new state
/// survived a complete eligible ceremony.
fn boundary_covers_receipt(boundary: &BoundaryWitness, receipt_height: u64) -> bool {
    boundary.freeze >= receipt_height
}

fn validate_boundary_predecessor(
    world: &World,
    ports: &[u16],
    boundary: &BoundaryWitness,
    epoch: u64,
) -> Result<()> {
    let previous = boundary
        .planned
        .checked_sub(epoch)
        .ok_or_else(|| eyre!("invalid planned activation"))?;
    if previous > 0 {
        ensure!(
            dkg_boundary_at(world, ports, previous)?.is_some(),
            "DKG schedule is not anchored to a canonical previous activation"
        );
    }
    let old_epoch = dkg_epoch_at(world, ports[0], boundary.freeze)?;
    ensure!(
        boundary.epoch
            == old_epoch
                .checked_add(1)
                .ok_or_else(|| eyre!("epoch overflow"))?,
        "readiness boundary did not advance its pinned predecessor epoch once"
    );
    Ok(())
}

#[then("the unconfirmed joiner stays pending across a full reshare cycle")]
fn stays_pending(world: &mut World) {
    observe_readiness_cycle(world, false)
        .expect("unconfirmed joiner remains excluded through one complete finalized cycle");
}

#[when("the joiner confirms readiness")]
fn joiner_confirms(world: &mut World) {
    let index = world.validators.joiner_index();
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let outcome = world
        .rpc
        .confirm_ready_outcome(&key, index)
        .expect("explicit readiness transaction");
    let ports = dkg_ports(world, &[0, 1, 2, 3, 4]).expect("same five owners before confirmation");
    let point = world
        .rpc
        .finalize_outcome(&outcome, &ports, 40)
        .expect("canonical finalized confirmation");
    world.state.lifecycle_before = Some(point);
    world
        .state
        .restart_observations
        .push(json!({"phase": "readiness_confirm_receipt",
        "transaction_hash": outcome.transaction_hash, "receipt": outcome.receipt}));
    record_dkg_checkpoint(world, "readiness_confirm_finalized", &ports, point);
}

#[then("the confirmed joiner activates on the next reshare")]
fn confirmed_joiner_activates(world: &mut World) {
    observe_readiness_cycle(world, true)
        .expect("confirmation activates at its matching finalized boundary");
    let joiner = world.validators.joiner_index();
    let log = world
        .state
        .lifecycle_incarnations
        .get_mut(&joiner)
        .expect("owned readiness log")
        .node_log
        .read()
        .expect("required current-process readiness log");
    assert!(
        !log.contains("attributable invalid VRF seed partial"),
        "joiner rejected the active committee's VRF partials"
    );
    assert!(
        !log.contains("finalized certificate carries an unverifiable VRF proof"),
        "joiner could not verify finalized VRF proof"
    );
    assert_eq!(
        world
            .rpc
            .has_threshold_shares(world.validators.http_port(joiner)),
        Some(true),
        "activated readiness joiner has no loaded threshold share"
    );
    dkg_ports(world, &[0, 1, 2, 3, 4]).expect("readiness owners remain unchanged at completion");
}

fn observe_readiness_cycle(world: &mut World, confirmed: bool) -> Result<()> {
    let anchor = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing finalized readiness transaction anchor"))?;
    let (epoch, prepare) = readiness_schedule(world)?;
    let members = dkg_addresses(world, 5)?;
    let ports = dkg_ports(world, &[0, 1, 2, 3, 4])?;
    let deadline = Instant::now() + Duration::from_secs(400);
    let mut fresh = world.rpc.fresh_finality_target(&ports)?;
    let mut next = anchor.height;
    let mut matched: Option<BoundaryWitness> = None;
    loop {
        let ports = dkg_ports(world, &[0, 1, 2, 3, 4])?;
        let point = world.rpc.wait_finalized_checkpoint(&ports, 0, 1)?;
        while next <= point.height {
            ensure!(
                Instant::now() < deadline,
                "readiness cycle proof exceeded its existing observation budget"
            );
            if let Some(boundary) = dkg_boundary_at(world, &ports, next)? {
                validate_readiness_boundary(&boundary, epoch, prepare)?;
                validate_boundary_predecessor(world, &ports, &boundary, epoch)?;
                if let Some(previous) = &matched {
                    ensure!(
                        boundary.cycle != previous.cycle && boundary.epoch != previous.epoch,
                        "readiness target activated more than once"
                    );
                } else if boundary_covers_receipt(&boundary, anchor.height) {
                    let mut actual = boundary.members.clone();
                    actual.sort_unstable();
                    let mut expected = members[..if confirmed { 5 } else { 4 }].to_vec();
                    expected.sort_unstable();
                    ensure!(
                        actual == expected,
                        "first eligible readiness boundary has the wrong membership"
                    );
                    world.state.restart_observations.push(json!({"phase": "readiness_cycle_boundary",
                        "confirmed": confirmed, "receipt_height": anchor.height, "epoch_length": epoch,
                        "prepare_window": prepare, "boundary": boundary}));
                    matched = Some(boundary);
                    fresh = world.rpc.fresh_finality_target(&ports)?;
                }
            }
            let active = confirmed && matched.is_some();
            let at = world.rpc.checkpoint_at(ports[0], next)?;
            dkg_membership_at(
                world,
                &ports,
                at,
                &members[..if active { 5 } else { 4 }],
                members[4],
                if active { 2 } else { 1 },
            )?;
            next = next
                .checked_add(1)
                .ok_or_else(|| eyre!("readiness scan height overflow"))?;
        }
        if matched.is_some() && point.height >= fresh {
            dkg_ports(world, &[0, 1, 2, 3, 4])?;
            record_dkg_checkpoint(
                world,
                if confirmed {
                    "readiness_active_finality"
                } else {
                    "readiness_pending_finality"
                },
                &ports,
                point,
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "no full finalized readiness cycle within the existing observation budget"
        );
        sleep(Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    fn boundary() -> BoundaryWitness {
        BoundaryWitness {
            height: 247,
            block_hash: B256::repeat_byte(2),
            state_root: B256::repeat_byte(3),
            epoch: 3,
            cycle: 2,
            freeze: 210,
            planned: 240,
            target_hash: B256::repeat_byte(1),
            members: Vec::new(),
        }
    }

    #[test]
    fn readiness_requires_an_entire_cycle_after_the_actual_receipt() {
        let value = boundary();
        assert!(boundary_covers_receipt(&value, 210));
        assert!(!boundary_covers_receipt(&value, 211));
        assert!(!boundary_covers_receipt(&value, 241));
        assert!(!boundary_covers_receipt(&value, 10_000));
    }

    #[test]
    fn readiness_uses_executed_schedule_and_accepts_delayed_activation() {
        let value = boundary();
        validate_readiness_boundary(&value, 120, 30).unwrap();
        for (epoch, prepare) in [(0, 30), (120, 0), (120, 31), (20, 30)] {
            assert!(validate_readiness_boundary(&value, epoch, prepare).is_err());
        }
        let mut early = value.clone();
        early.height = 239;
        assert!(validate_readiness_boundary(&early, 120, 30).is_err());
        let mut unknown = value;
        unknown.target_hash = B256::ZERO;
        assert!(validate_readiness_boundary(&unknown, 120, 30).is_err());
    }
}
