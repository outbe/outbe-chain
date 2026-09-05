//! Validator lifecycle consistency scenarios whose observable boundary is the
//! periodic DKG scheduler.
//!
//! These steps deliberately use only valid genesis profiles and public
//! transactions/RPC reads. In particular, `pending_set_change` is observed as a
//! durable on-chain hint; it is never used to force the scheduler.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{keccak256, Address, B256};
use cucumber::{given, then, when};

use crate::features::common::start_bootstrapped_localnet;
use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::World;

const EPOCH_BLOCKS: u64 = 60;
const PREPARE_BLOCKS: u64 = 20;
const ACTIVATION_GRACE_BLOCKS: u64 = 30;
const CEREMONY_LOG: &str = "freezing validator set and starting DKG rotation";

fn boot_profiled_localnet(world: &mut World, owner: Option<usize>) {
    let mut profile = BootstrapProfile::default()
        .with_dkg_timing(EPOCH_BLOCKS, PREPARE_BLOCKS, ACTIVATION_GRACE_BLOCKS)
        .expect("valid short DKG timing")
        .with_dev_felony_threshold(EPOCH_BLOCKS - 1)
        .expect("valid felony threshold");
    if let Some(owner) = owner {
        profile = profile.with_validator_set_owner(owner);
    }

    let committee_size = world.validators.size();
    assert_eq!(
        committee_size, 4,
        "validator consistency fixtures require exactly four genesis validators"
    );
    world.state.voting_window = 6;
    world.state.wwd = Some(crate::world::localnet::worldwide_day());
    world
        .localnet
        .bootstrap_with_profile(committee_size, &profile)
        .expect("bootstrap profiled localnet");
    start_bootstrapped_localnet(world, &StartOpts::with_voting_window(6));
}

fn planned_activation(world: &World) -> u64 {
    world
        .rpc
        .consensus_status_field(
            world.validators.primary_port(),
            "nextPlannedActivationHeight",
        )
        .and_then(|value| value.trim_matches('"').parse::<u64>().ok())
        .expect("next planned activation height")
}

fn validator_address(world: &World, name: &str) -> String {
    let key = world
        .validators
        .by_name(name)
        .expect("validator name")
        .evm_key()
        .expect("validator key");
    world.rpc.address_of(&key).expect("validator address")
}

fn active_set_hash(addresses: &[Address]) -> B256 {
    let mut encoded = Vec::with_capacity(8 + addresses.len() * 20);
    encoded.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        encoded.extend_from_slice(address.as_slice());
    }
    keccak256(encoded)
}

fn wait_for_schedule_advance(world: &World, previous: u64) -> u64 {
    let primary = world.validators.primary_port();
    for _ in 0..180 {
        if let Some(next) = world
            .rpc
            .consensus_status_field(primary, "nextPlannedActivationHeight")
            .and_then(|value| value.trim_matches('"').parse::<u64>().ok())
        {
            if next > previous {
                return next;
            }
        }
        sleep(Duration::from_secs(2));
    }
    panic!("DKG schedule did not advance beyond activation {previous}");
}

fn wait_for_safe_pre_freeze_window(world: &World) -> u64 {
    let primary = world.validators.primary_port();
    for _ in 0..4 {
        let activation = planned_activation(world);
        let freeze = activation.saturating_sub(PREPARE_BLOCKS);
        let head = world.rpc.head(primary).expect("head before join setup");
        if head.saturating_add(10) < freeze {
            return activation;
        }
        let _ = world
            .rpc
            .wait_block_gt(primary, activation, 90)
            .expect("advance beyond crowded DKG window");
        let _ = wait_for_schedule_advance(world, activation);
    }
    panic!("no safe pre-freeze interval became available across four DKG schedules");
}

fn wait_until_all_nodes_observe_height(world: &World, height: u64) {
    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, height, 60),
            "RPC {port} did not finalize height {height}"
        );
    }
}

// ---- D-02: a post-freeze readiness signal waits for the periodic scheduler --

#[given("a fresh localnet with a short valid DKG schedule")]
fn short_valid_dkg_schedule(world: &mut World) {
    boot_profiled_localnet(world, None);
}

#[given("a staked joiner confirms readiness immediately after the target freeze")]
fn confirm_joiner_after_freeze(world: &mut World) {
    let primary = world.validators.primary_port();
    let joiner_index = world.validators.joiner_index();

    world
        .localnet
        .provision_joiner(joiner_index)
        .expect("provision post-freeze joiner");
    world
        .localnet
        .launch_caught_up_joiner(joiner_index, &[])
        .expect("launch post-freeze joiner");

    let key = world.validators.joiner().evm_key().expect("joiner key");
    let address = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(address.clone());
    world.rpc.stake(&key, 1000).expect("stake joiner");
    assert_eq!(
        world.rpc.validator_status(primary, &address),
        Some(1),
        "staked joiner must be PENDING"
    );

    // Wait inside the current post-freeze/pre-activation interval. If
    // provisioning happened to consume the whole interval, use the next valid
    // periodic window rather than racing the boundary.
    let mut selected_activation = None;
    for _ in 0..600 {
        let candidate = planned_activation(world);
        let freeze = candidate.saturating_sub(PREPARE_BLOCKS);
        let head = world
            .rpc
            .head(primary)
            .expect("head before post-freeze readiness");
        if head >= freeze.saturating_add(2) && head.saturating_add(6) < candidate {
            selected_activation = Some(candidate);
            break;
        }
        if head.saturating_add(6) >= candidate {
            let _ = world
                .rpc
                .wait_block_gt(primary, candidate, 90)
                .expect("advance to next DKG schedule");
            let _ = wait_for_schedule_advance(world, candidate);
        } else {
            sleep(Duration::from_secs(2));
        }
    }
    let activation =
        selected_activation.expect("no post-freeze readiness interval became available");

    for _ in 0..120 {
        if world
            .localnet
            .log_count(0, CEREMONY_LOG)
            .expect("read required owned process log")
            > 0
        {
            break;
        }
        sleep(Duration::from_millis(500));
    }
    let ceremony_count = world
        .localnet
        .log_count(0, CEREMONY_LOG)
        .expect("read required owned process log");
    assert!(
        ceremony_count > 0,
        "the target freeze was not observed before readiness confirmation"
    );

    let outcome = world
        .rpc
        .confirm_ready_outcome(&key, joiner_index)
        .expect("submit post-freeze readiness");
    assert!(
        outcome.success,
        "post-freeze readiness transaction reverted"
    );
    assert_eq!(
        world.rpc.validator_status(primary, &address),
        Some(1),
        "post-freeze readiness activated the joiner out of schedule"
    );
    assert!(
        !world
            .rpc
            .is_participant(primary, &address)
            .expect("observe consensus participation"),
        "post-freeze readiness made the joiner an immediate participant"
    );

    world.state.activation_height = Some(activation);
    world.state.marker_count = Some(ceremony_count);
}

#[when("the joiner set-change flag becomes observable")]
fn joiner_set_change_is_observable(world: &mut World) {
    let primary = world.validators.primary_port();
    for _ in 0..30 {
        if world
            .rpc
            .has_pending_set_change(primary)
            .expect("observe pending-set-change signal")
        {
            return;
        }
        sleep(Duration::from_secs(1));
    }
    panic!("post-freeze readiness never made pending_set_change observable");
}

#[then("no new DKG ceremony starts before the next periodic window")]
fn no_out_of_schedule_dkg(world: &mut World) {
    let primary = world.validators.primary_port();
    let address = world.state.joiner_addr.clone().expect("joiner address");
    let first_activation = world
        .state
        .activation_height
        .expect("first scheduled activation");
    let ceremony_count = world.state.marker_count.expect("freeze ceremony count");

    // The already-frozen same-membership rotation must finish first. Its epoch
    // restart publishes the next periodic activation height.
    let mut next_activation = None;
    for _ in 0..180 {
        let observed_activation = planned_activation(world);
        if observed_activation > first_activation {
            next_activation = Some(observed_activation);
            break;
        }
        assert_eq!(
            world
                .localnet
                .log_count(0, CEREMONY_LOG)
                .expect("read required owned process log"),
            ceremony_count,
            "readiness started an immediate extra DKG ceremony"
        );
        assert!(
            !world
                .rpc
                .is_participant(primary, &address)
                .expect("observe consensus participation"),
            "joiner partially activated at the already-frozen boundary"
        );
        sleep(Duration::from_secs(2));
    }
    let next_activation =
        next_activation.expect("periodic activation schedule did not advance after 360 seconds");
    let pre_freeze_height = next_activation
        .saturating_sub(PREPARE_BLOCKS)
        .saturating_sub(2);
    while world
        .rpc
        .head(primary)
        .expect("primary head before next periodic freeze")
        < pre_freeze_height
    {
        assert_eq!(
            world
                .localnet
                .log_count(0, CEREMONY_LOG)
                .expect("read required owned process log"),
            ceremony_count,
            "readiness started DKG before the next periodic freeze"
        );
        assert!(
            !world
                .rpc
                .is_participant(primary, &address)
                .expect("observe consensus participation"),
            "joiner became a participant before the next periodic freeze"
        );
        sleep(Duration::from_secs(2));
    }
    assert_eq!(
        world
            .localnet
            .log_count(0, CEREMONY_LOG)
            .expect("read required owned process log"),
        ceremony_count,
        "an extra ceremony started before the next periodic window"
    );
    world.state.activation_height = Some(next_activation);
}

#[then("the flag remains set until a matching boundary activates the joiner")]
fn pending_flag_survives_until_matching_boundary(world: &mut World) {
    for port in world.validators.committee_ports() {
        assert!(
            world
                .rpc
                .has_pending_set_change(port)
                .expect("observe pending-set-change signal"),
            "RPC {port} cleared pending_set_change before a boundary containing the joiner"
        );
    }
}

#[then("the joiner becomes active no later than that scheduled DKG window")]
fn joiner_activates_in_scheduled_window(world: &mut World) {
    let primary = world.validators.primary_port();
    let address = world.state.joiner_addr.clone().expect("joiner address");
    let scheduled = world
        .state
        .activation_height
        .expect("matching activation height");
    let deadline = scheduled.saturating_add(ACTIVATION_GRACE_BLOCKS);

    for _ in 0..180 {
        if world
            .rpc
            .is_participant(primary, &address)
            .expect("observe consensus participation")
        {
            let height = world.rpc.head(primary).expect("activation height");
            assert!(
                height <= deadline,
                "joiner activated after the documented DKG window: {height} > {deadline}"
            );
            for port in world.validators.committee_ports() {
                assert_eq!(
                    world.rpc.validator_status(port, &address),
                    Some(2),
                    "RPC {port} did not observe ACTIVE"
                );
                assert_eq!(
                    world.rpc.has_share(port, &address),
                    Some(true),
                    "RPC {port} did not observe the installed share"
                );
                assert!(
                    world
                        .rpc
                        .is_participant(port, &address)
                        .expect("observe consensus participation"),
                    "RPC {port} did not admit the activated joiner"
                );
            }
            return;
        }

        assert!(
            world
                .rpc
                .has_pending_set_change(primary)
                .expect("observe pending-set-change signal"),
            "pending_set_change cleared while the confirmed joiner was still omitted"
        );
        let head = world
            .rpc
            .head(primary)
            .expect("primary head during scheduled activation window");
        assert!(
            head <= deadline,
            "joiner missed the scheduled DKG activation window at height {head}"
        );
        sleep(Duration::from_secs(2));
    }
    panic!("joiner never became ACTIVE in the scheduled DKG window");
}

// ---- D-04: external owners cannot manufacture an ACTIVE/shareless member ----

#[given("a fresh localnet whose four active validators have BLS shares")]
fn four_active_validators_with_shares(world: &mut World) {
    boot_profiled_localnet(world, Some(0));
    let primary = world.validators.primary_port();
    assert!(
        world.rpc.wait_block(primary, 5, 30).is_some(),
        "committee did not reach a usable height"
    );

    for index in 0..4 {
        let address = validator_address(world, &format!("validator-{index}"));
        assert_eq!(
            world.rpc.validator_status(primary, &address),
            Some(2),
            "validator-{index} is not ACTIVE"
        );
        assert_eq!(
            world.rpc.has_share(primary, &address),
            Some(true),
            "validator-{index} lacks its genesis share"
        );
        assert!(
            world
                .rpc
                .is_participant(primary, &address)
                .expect("observe consensus participation"),
            "validator-{index} is not a consensus participant"
        );
    }
}

#[when(
    expr = "the configured owner attempts to activate a canonical reshared set omitting {string}"
)]
fn owner_attempts_to_omit_active_validator(world: &mut World, omitted_name: String) {
    let primary = world.validators.primary_port();
    let omitted: Address = validator_address(world, &omitted_name)
        .parse()
        .expect("omitted validator address");
    let mut active_set = world
        .rpc
        .active_consensus_set(primary)
        .expect("current active consensus set");
    assert_eq!(active_set.len(), 4, "expected a four-member live set");
    active_set.retain(|address| *address != omitted);
    assert_eq!(active_set.len(), 3, "omitted validator was not in live set");

    let owner_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("ValidatorSet owner key");
    let outcome = world
        .rpc
        .activate_reshared_set(&owner_key, &active_set, active_set_hash(&active_set))
        .expect("submit forbidden owner boundary");
    assert!(
        !outcome.success,
        "owner omission unexpectedly succeeded: {}",
        outcome.transaction_hash
    );
    let block = outcome.block_number().expect("owner omission block");
    wait_until_all_nodes_observe_height(world, block);
}

#[then("the owner omission is rejected with all four validators still ACTIVE with shares")]
fn owner_omission_is_rejected_atomically(world: &mut World) {
    for index in 0..4 {
        let validator_name = format!("validator-{index}");
        let address = validator_address(world, &validator_name);
        for port in world.validators.committee_ports() {
            assert_eq!(
                world.rpc.validator_status(port, &address),
                Some(2),
                "{validator_name} is not ACTIVE on RPC {port}"
            );
            assert_eq!(
                world.rpc.has_share(port, &address),
                Some(true),
                "{validator_name} lost its share on RPC {port}"
            );
            assert!(
                world
                    .rpc
                    .is_participant(port, &address)
                    .expect("observe consensus participation"),
                "{validator_name} stopped participating on RPC {port}"
            );
        }
    }
}

#[then("committee membership and state roots converge on every validator")]
fn committee_converges(world: &mut World) {
    let ports = world.validators.committee_ports();
    let common_height = world
        .rpc
        .finalized(ports[0])
        .expect("primary finalization after rejected owner omission");
    wait_until_all_nodes_observe_height(world, common_height);
    let expected_members = world
        .rpc
        .active_consensus_set(ports[0])
        .expect("primary consensus set");
    assert_eq!(
        expected_members.len(),
        4,
        "consensus set changed after rejected owner omission"
    );
    let expected_root = world
        .rpc
        .state_root(ports[0], common_height)
        .expect("primary state root");

    for port in ports {
        assert_eq!(
            world.rpc.active_consensus_set(port),
            Some(expected_members.clone()),
            "committee membership diverged on RPC {port}"
        );
        assert_eq!(
            world.rpc.state_root(port, common_height),
            Some(expected_root.clone()),
            "state root diverged on RPC {port} at height {common_height}"
        );
    }
}

// ---- S-01: readiness cannot survive PENDING -> REGISTERED -> PENDING --------

#[given("a staked PENDING joiner has confirmed readiness")]
fn confirmed_pending_joiner(world: &mut World) {
    boot_profiled_localnet(world, None);
    let primary = world.validators.primary_port();
    let joiner_index = world.validators.joiner_index();

    world
        .localnet
        .provision_joiner(joiner_index)
        .expect("provision readiness-reset joiner");
    world
        .localnet
        .launch_caught_up_joiner(joiner_index, &[])
        .expect("launch readiness-reset joiner");

    // Avoid racing the transition under test with an already-frozen target.
    let _ = wait_for_safe_pre_freeze_window(world);
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let address = world.rpc.address_of(&key).expect("joiner address");
    world.state.joiner_addr = Some(address.clone());
    world.rpc.stake(&key, 1000).expect("stake joiner");
    let ready = world
        .rpc
        .confirm_ready_outcome(&key, joiner_index)
        .expect("confirm initial readiness");
    assert!(ready.success, "initial readiness confirmation reverted");
    assert_eq!(
        world.rpc.validator_status(primary, &address),
        Some(1),
        "joiner is not PENDING after readiness confirmation"
    );
    assert_eq!(
        world.rpc.has_share(primary, &address),
        Some(false),
        "PENDING joiner already has a share"
    );
}

#[when("it unstakes below minimum before activation")]
fn pending_joiner_unstakes_below_minimum(world: &mut World) {
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let outcome = world.rpc.unstake(&key, 1).expect("unstake below minimum");
    assert!(outcome.success, "pending unstake reverted");
}

#[then("it is REGISTERED with readiness cleared")]
fn pending_joiner_returns_to_registered(world: &mut World) {
    let primary = world.validators.primary_port();
    let address = world.state.joiner_addr.clone().expect("joiner address");
    assert_eq!(
        world.rpc.validator_status(primary, &address),
        Some(0),
        "below-minimum PENDING joiner did not return to REGISTERED"
    );
    assert_eq!(
        world.rpc.has_share(primary, &address),
        Some(false),
        "REGISTERED joiner retained a share"
    );
    assert!(
        !world
            .rpc
            .is_participant(primary, &address)
            .expect("observe consensus participation"),
        "REGISTERED joiner remained a participant"
    );
}

#[when("it stakes again without another readiness confirmation")]
fn restake_without_readiness(world: &mut World) {
    let primary = world.validators.primary_port();
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let address = world.state.joiner_addr.clone().expect("joiner address");
    world.rpc.stake(&key, 1).expect("restore minimum stake");
    assert_eq!(
        world.rpc.validator_status(primary, &address),
        Some(1),
        "restaked joiner did not return to PENDING"
    );

    let current_activation = planned_activation(world);
    let freeze = current_activation.saturating_sub(PREPARE_BLOCKS);
    let head = world.rpc.head(primary).expect("head after restake");
    let relevant_activation = if head.saturating_add(3) < freeze {
        current_activation
    } else {
        wait_for_schedule_advance(world, current_activation)
    };
    world.state.activation_height = Some(relevant_activation);
}

#[then("it remains PENDING and excluded after the next scheduled reshare")]
fn restaked_joiner_stays_excluded(world: &mut World) {
    let primary = world.validators.primary_port();
    let address = world.state.joiner_addr.clone().expect("joiner address");
    let activation = world
        .state
        .activation_height
        .expect("relevant activation height");
    let observation_height = activation.saturating_add(2);
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, observation_height, 120),
        "chain did not finalize across the scheduled reshare at {activation}"
    );

    assert_eq!(
        world.rpc.validator_status(primary, &address),
        Some(1),
        "stale readiness promoted the restaked joiner"
    );
    assert_eq!(
        world.rpc.has_share(primary, &address),
        Some(false),
        "restaked joiner received a share without reconfirmation"
    );
    assert!(
        !world
            .rpc
            .is_participant(primary, &address)
            .expect("observe consensus participation"),
        "restaked joiner participates without reconfirmation"
    );
    assert_eq!(
        world.rpc.active_count(primary),
        Some(4),
        "unconfirmed restake changed the ACTIVE set"
    );
}
