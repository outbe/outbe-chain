//! One live, pinned committee handoff across both chained followers and restart.
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U64;
use cucumber::{then, when};
use eyre::{ensure, eyre, Result};
use outbe_evm::tee_attestation_activation::DcapSeededChainSpecBindingV1;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

use crate::internal::certified_handoff::{read_certified, AuthenticatedHistory, PinnedHandoff};
use crate::internal::eth;
use crate::world::{rpc::Rpc, World};

const FOLLOWERS: [(&str, usize); 2] = [("follower", 14), ("follower2", 15)];

fn all_ports(world: &World) -> Vec<u16> {
    let mut ports = world.validators.committee_ports();
    ports.extend(FOLLOWERS.map(|(_, slot)| world.validators.http_port(slot)));
    ports
}

fn follower_pids(world: &mut World) -> Result<[u32; 2]> {
    Ok([
        world.localnet.live_follower_pid(FOLLOWERS[0].0)?,
        world.localnet.live_follower_pid(FOLLOWERS[1].0)?,
    ])
}

fn head(rpc: &Rpc, port: u16) -> Result<u64> {
    let number: U64 = serde_json::from_value(eth::raw_json_result(
        &rpc.url(port),
        "eth_blockNumber",
        serde_json::json!([]),
    )?)?;
    Ok(number.to())
}

fn advance_one(
    rpc: &Rpc,
    port: u16,
    history: &mut AuthenticatedHistory,
) -> Result<Option<ConsensusHeaderArtifact>> {
    let height = history
        .height()
        .checked_add(1)
        .ok_or_else(|| eyre!("history height overflow"))?;
    let proof = read_certified(rpc, port, height, history.member_count())?;
    history.advance(&proof, rpc.checkpoint_at(port, height)?)
}

fn advance_through(
    rpc: &Rpc,
    port: u16,
    history: &mut AuthenticatedHistory,
    height: u64,
) -> Result<()> {
    ensure!(
        rpc.finalized_result(port)? >= height,
        "requested history is not finalized"
    );
    while history.height() < height {
        advance_one(rpc, port, history)?;
    }
    Ok(())
}

/// Check canonical HEADs after collecting the local witnesses, not a possibly
/// lagging finalized tip. Missing the pinned handoff window fails the scenario.
fn require_boundary_future(world: &World, pinned: &PinnedHandoff) -> Result<()> {
    for port in world.validators.committee_ports() {
        let through = head(&world.rpc, port)?;
        // A successful HEAD below the carrier is lag, not a failed handoff.
        // Still inspect every other validator: lag must not hide a boundary.
        for height in pinned.carrier.height..=through {
            let (_, _, extra) = eth::block_commitment_result(&world.rpc.url(port), height)?;
            pinned.require_before_boundary(&extra)?;
        }
    }
    Ok(())
}

#[then("both live followers authenticate one new preannounce before its boundary")]
fn pin_live_handoff(world: &mut World) {
    let ports = all_ports(world);
    world
        .localnet
        .ensure_committee_alive()
        .expect("live committee before handoff");
    let pids = follower_pids(world).expect("both owned FullNodes alive before pin selection");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 90)
        .expect("initial exact checkpoint on all six nodes");
    let binding = DcapSeededChainSpecBindingV1::from_genesis_path(
        &world.localnet.scenario_dir().join("genesis.json"),
    )
    .map_err(|error| eyre!(error))
    .expect("trusted materialized genesis binding");
    for port in &ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(*port, 0)
                .expect("node genesis checkpoint")
                .block_hash,
            binding.genesis_hash
        );
    }
    let primary = world.validators.primary_port();
    let baseline = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| head(&world.rpc, port))
        .collect::<Result<Vec<_>>>()
        .expect("successful pre-selection canonical heads")
        .into_iter()
        .max()
        .expect("nonempty committee");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, baseline, 90)
        .expect("both followers reach fixed pre-selection history");
    let mut history = AuthenticatedHistory::new(&binding).expect("genesis committee verifier");
    advance_through(&world.rpc, primary, &mut history, baseline)
        .expect("authenticate every transition from genesis");
    let wanted = history
        .highest_registered_epoch()
        .unwrap()
        .checked_add(1)
        .expect("successor epoch");
    let deadline = Instant::now() + Duration::from_secs(360);
    let pinned = loop {
        assert!(
            Instant::now() < deadline,
            "no future preannounce for pinned epoch {wanted}"
        );
        assert_eq!(
            follower_pids(world).unwrap(),
            pids,
            "follower changed before live handoff"
        );
        let through = world
            .rpc
            .finalized_result(primary)
            .expect("primary finalized history");
        let mut found = None;
        while history.height() < through {
            if let Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, outcome }) =
                advance_one(&world.rpc, primary, &mut history)
                    .expect("authenticate next history block")
            {
                assert!(epoch <= wanted, "skipped pinned successor epoch");
                if epoch == wanted {
                    found = Some(outcome);
                    break;
                }
            }
        }
        if let Some(outcome) = found {
            let carrier = world
                .rpc
                .checkpoint_at(primary, history.height())
                .expect("pinned canonical carrier");
            break PinnedHandoff {
                history,
                epoch: wanted,
                carrier,
                outcome,
                boundary: None,
                follower_pids: pids,
                survivor_anchor_after_fault: None,
                follower_watermarks: None,
            };
        }
        sleep(Duration::from_millis(100));
    };
    let mut witnessed = [false; 2];
    loop {
        assert!(
            Instant::now() < deadline,
            "local pinned finalizations never became available"
        );
        require_boundary_future(world, &pinned).expect("selected boundary must still be future");
        assert_eq!(follower_pids(world).unwrap(), pids);
        for (index, (_, slot)) in FOLLOWERS.iter().enumerate() {
            let port = world.validators.http_port(*slot);
            if !witnessed[index]
                && world
                    .rpc
                    .finalized_result(port)
                    .expect("follower finalized availability")
                    >= pinned.carrier.height
            {
                let proof = read_certified(
                    &world.rpc,
                    port,
                    pinned.carrier.height,
                    pinned.history.member_count(),
                )
                .expect("local marshal carrier proof");
                pinned
                    .verify_carrier(&proof)
                    .expect("follower authenticated the exact pinned preannounce");
                witnessed[index] = true;
            }
        }
        // This observation must follow both local proofs. A lagging validator
        // is already before the boundary; it must not delay valid witnesses.
        require_boundary_future(world, &pinned)
            .expect("both local witnesses precede every canonical successor boundary");
        if witnessed.iter().all(|done| *done) {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    // Keep the authenticated verifier and immutable carrier for the later
    // boundary/restart checks; never perform a fresh latest-handoff selection.
    assert_eq!(follower_pids(world).unwrap(), pids);
    world.state.chained_handoff = Some(pinned);
}

fn verify_both_pinned(world: &mut World, pinned: &PinnedHandoff) -> Result<()> {
    follower_pids(world)?;
    for (_, slot) in FOLLOWERS {
        let port = world.validators.http_port(slot);
        let preannounce = read_certified(
            &world.rpc,
            port,
            pinned.carrier.height,
            pinned.history.member_count(),
        )?;
        pinned.verify_carrier(&preannounce)?;
        let boundary = pinned.boundary.ok_or_else(|| eyre!("no pinned boundary"))?;
        let proof = read_certified(
            &world.rpc,
            port,
            boundary.height,
            pinned.history.member_count(),
        )?;
        pinned.verify_boundary(&proof)?;
    }
    Ok(())
}

#[then("both followers finalize that exact successor boundary")]
fn confirm_pinned_boundary(world: &mut World) {
    let mut pinned = world
        .state
        .chained_handoff
        .take()
        .expect("live pinned handoff");
    let primary = world.validators.primary_port();
    let deadline = Instant::now() + Duration::from_secs(360);
    while pinned.boundary.is_none() {
        assert!(
            Instant::now() < deadline,
            "pinned successor never activated"
        );
        assert_eq!(follower_pids(world).unwrap(), pinned.follower_pids);
        let through = world
            .rpc
            .finalized_result(primary)
            .expect("primary boundary progress");
        while pinned.history.height() < through {
            if let Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)) =
                advance_one(&world.rpc, primary, &mut pinned.history)
                    .expect("authenticated boundary history")
            {
                assert!(boundary.epoch <= pinned.epoch, "pinned boundary skipped");
                if boundary.epoch == pinned.epoch {
                    assert_eq!(
                        boundary.outcome, pinned.outcome,
                        "successor changed the pinned DKG outcome"
                    );
                    pinned.boundary = Some(
                        world
                            .rpc
                            .checkpoint_at(primary, pinned.history.height())
                            .expect("canonical pinned boundary"),
                    );
                    break;
                }
            }
        }
        sleep(Duration::from_millis(100));
    }
    let target = pinned.boundary.unwrap().height.checked_add(2).unwrap();
    world
        .rpc
        .wait_finalized_checkpoint(&all_ports(world), target, 90)
        .expect("all six nodes finalized the pinned handoff");
    verify_both_pinned(world, &pinned).expect("both exact local successor certificates");
    world.state.chained_handoff = Some(pinned);
}

#[when("both chained followers lose their only live upstream while quorum advances")]
fn disconnect_chain(world: &mut World) {
    let ports = all_ports(world);
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 90)
        .expect("all-node checkpoint immediately before upstream fault");
    let watermarks = FOLLOWERS.map(|(_, slot)| {
        let height = world
            .rpc
            .finalized_result(world.validators.http_port(slot))
            .expect("successful follower watermark before upstream fault");
        require_monotonic([checkpoint.height; 2], [height; 2])
            .expect("pre-fault finality cannot regress behind the common checkpoint");
        height
    });
    world
        .state
        .chained_handoff
        .as_mut()
        .unwrap()
        .follower_watermarks = Some(watermarks);
    let pids = follower_pids(world).expect("both followers alive before fault");
    assert_eq!(
        pids,
        world.state.chained_handoff.as_ref().unwrap().follower_pids
    );
    world
        .localnet
        .kill_validator(0)
        .expect("stop the configured upstream");
    let survivor = world.validators.http_port(1);
    let anchor = world
        .rpc
        .finalized_result(survivor)
        .expect("surviving finality after source exit");
    world
        .state
        .chained_handoff
        .as_mut()
        .unwrap()
        .survivor_anchor_after_fault = Some(anchor);
}

fn require_monotonic(previous: [u64; 2], observed: [u64; 2]) -> Result<()> {
    for index in 0..2 {
        ensure!(
            observed[index] >= previous[index],
            "follower {index} finalized height regressed: {} -> {}",
            previous[index],
            observed[index]
        );
    }
    Ok(())
}

#[then("both disconnected followers exhaust only authenticated backlog and stop advancing")]
fn disconnected_chain_stays_certified(world: &mut World) {
    let mut pinned = world
        .state
        .chained_handoff
        .take()
        .expect("pinned handoff before fault");
    let survivor = world.validators.http_port(1);
    let surviving_ports: Vec<_> = world
        .validators
        .committee_ports()
        .into_iter()
        .skip(1)
        .collect();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last_checked = [pinned.boundary.unwrap().height; 2];
    let mut last_observed = pinned.follower_watermarks.expect("pre-fault watermarks");
    loop {
        assert!(
            Instant::now() < deadline,
            "followers never drained their certified backlog"
        );
        assert_eq!(follower_pids(world).unwrap(), pinned.follower_pids);
        let before = FOLLOWERS.map(|(_, slot)| {
            world
                .rpc
                .finalized_result(world.validators.http_port(slot))
                .expect("disconnected follower remains readable")
        });
        require_monotonic(last_observed, before).expect("monotonic pre-wait finalized tips");
        let target = before
            .iter()
            .copied()
            .chain([
                world.rpc.finalized_result(survivor).unwrap(),
                pinned.survivor_anchor_after_fault.unwrap(),
            ])
            .max()
            .unwrap()
            .checked_add(5)
            .unwrap();
        let checkpoint = world
            .rpc
            .wait_finalized_checkpoint(&surviving_ports, target, 60)
            .expect("surviving quorum continues while both followers are disconnected");
        advance_through(&world.rpc, survivor, &mut pinned.history, checkpoint.height)
            .expect("authenticate live surviving chain");
        let after = FOLLOWERS.map(|(_, slot)| {
            world
                .rpc
                .finalized_result(world.validators.http_port(slot))
                .expect("read both disconnected follower tips")
        });
        require_monotonic(before, after).expect("monotonic post-wait finalized tips");
        last_observed = after;
        for (index, (_, slot)) in FOLLOWERS.iter().enumerate() {
            assert!(
                after[index] >= last_checked[index] && after[index] <= checkpoint.height,
                "follower regressed or escaped the certified survivor horizon"
            );
            for height in last_checked[index] + 1..=after[index] {
                let proof = read_certified(
                    &world.rpc,
                    world.validators.http_port(*slot),
                    height,
                    pinned.history.member_count(),
                )
                .expect("local proof for every buffered finalized block");
                pinned
                    .history
                    .verify_retained(&proof, world.rpc.checkpoint_at(survivor, height).unwrap())
                    .expect("buffered follower progress has exact canonical finalization");
            }
            last_checked[index] = after[index];
        }
        if before == after {
            break;
        }
    }
    pinned.follower_watermarks = Some(last_observed);
    world.state.chained_handoff = Some(pinned);
}

#[cfg(test)]
mod tests {
    use super::require_monotonic;

    #[test]
    fn outage_observations_reject_regression_of_either_follower() {
        require_monotonic([110, 112], [110, 113]).unwrap();
        assert!(require_monotonic([110, 112], [105, 113]).is_err());
        assert!(require_monotonic([110, 112], [111, 105]).is_err());
        // An old boundary at 100 must not replace the pre-fault/latest tip.
        assert!(require_monotonic([110, 112], [105, 105]).is_err());
    }
}

#[when("both followers restart in place with the first switched to a healthy upstream")]
fn restart_both_followers(world: &mut World) {
    let old = world.state.chained_handoff.as_ref().unwrap().follower_pids;
    assert_eq!(follower_pids(world).unwrap(), old);
    world
        .localnet
        .restart_validator(0)
        .expect("restore only the stopped committee upstream");
    world
        .localnet
        .stop_follower("follower2")
        .expect("stop downstream follower");
    world
        .localnet
        .stop_follower("follower")
        .expect("stop upstream follower");
    // No provisioning/registration, key regeneration or datadir movement.
    world
        .localnet
        .launch_dcap_full_node("follower", 14, 1)
        .expect("restart same follower against healthy validator1");
    world
        .localnet
        .launch_dcap_full_node("follower2", 15, 14)
        .expect("restart same follower2 still chained through follower1");
    let ports = all_ports(world);
    world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 90)
        .expect("all restarted RPCs return at one exact finalized checkpoint");
    let new = follower_pids(world).expect("both restarted owned followers alive");
    assert_ne!(new[0], old[0]);
    assert_ne!(new[1], old[1]);
    world.state.chained_handoff.as_mut().unwrap().follower_pids = new;
}

#[then(
    "both restarted followers retain the same authenticated handoff and fresh six-node finality"
)]
fn verify_restarted_handoff(world: &mut World) {
    let mut pinned = world
        .state
        .chained_handoff
        .take()
        .expect("same live-pinned handoff");
    let ports = all_ports(world);
    let target = world
        .rpc
        .fresh_finality_target(&ports)
        .expect("post-repair anchor from every node");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, target, 90)
        .expect("fresh max+2 exact finality on all six nodes after both restarts");
    advance_through(
        &world.rpc,
        world.validators.primary_port(),
        &mut pinned.history,
        checkpoint.height,
    )
    .expect("authenticate every intervening committee transition after recovery");
    verify_both_pinned(world, &pinned)
        .expect("both restarted followers serve the SAME preannounce and successor certificate");
    assert_eq!(follower_pids(world).unwrap(), pinned.follower_pids);
    world
        .localnet
        .ensure_committee_alive()
        .expect("committee remains alive after complete repair");
    world.state.chained_handoff = Some(pinned);
}
