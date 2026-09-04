//! DKG failure steps used by `features/validator_lifecycle.feature`.
//! The DKG-failure feature. Freeze a 4->5 reshare target, then take the
//! joiner AND one committee validator offline so the ceremony begins with only
//! 3 online players (< player_threshold) and cannot complete. The OLD committee
//! keeps finalizing on its 3-of-4 quorum (no hard-halt); restoring the downed
//! validator lets a later retry complete and the set reaches 5.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then, when};

use crate::features::common::boot_localnet;
use crate::world::World;

/// Setup with a WIDE DKG activation grace so the VRF window outlasts the failed
/// reshare and RECOVERY can be demonstrated (s5:17-25).
#[given("a fresh localnet with a wide DKG activation grace")]
fn tuned_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "180".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "100".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "600".to_string()),
            // Keep validator-3 ACTIVE through the real 120-second failed-DKG
            // timeout. The default E2E threshold would jail it first, silently
            // turning the intended 4->5 target into a 4-member replacement
            // target whose three online players can complete DKG without a retry.
            ("TESTNET_DEV_FELONY_THRESHOLD", "179".to_string()),
        ],
    );
}

/// A compact live network for the permanent-loss safety path. The joiner can
/// sync and confirm before height 40; the frozen target then expires shortly
/// after its height-60 activation boundary.
#[given("a fresh localnet with a short DKG activation grace")]
fn short_grace_setup(world: &mut World) {
    boot_localnet(
        world,
        6,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "60".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "20".to_string()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "6".to_string()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "59".to_string()),
        ],
    );
}

/// Stake + confirm a joiner to freeze a 4->5 reshare target (s5:31-35).
#[when("a staked joiner freezes a 4-to-5 reshare target")]
fn freeze_target(world: &mut World) {
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_joiner(idx, &[])
        .expect("launch joiner");
    world
        .rpc
        .wait_block(joiner_port, 20, 40)
        .expect("joining node sync to height 20");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    world.rpc.confirm_ready(&key).expect("confirm ready");
}

/// Take the joiner + validator-3 offline BEFORE the ceremony so it begins with
/// only 3 online players and cannot complete (s5:37-49).
#[when("the reshare loses quorum before it can complete")]
fn lose_quorum(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    world.state.expected_dkg_reveal = Some(
        world
            .localnet
            .consensus_public_key(idx)
            .expect("derive offline joiner's consensus public key"),
    );
    world.state.marker_height = world.rpc.head(primary);
    world.localnet.stop_joiner(idx).expect("stop joiner");
    world.localnet.kill_validator(3).expect("kill validator-3");
}

#[when("the frozen-target joiner and one validator remain offline")]
fn lose_quorum_permanently(world: &mut World) {
    lose_quorum(world);
    // No ceremony completes in this path, so no share is reconstructed and the
    // recovery scenario's expected-reveal allowance must not apply.
    world.state.expected_dkg_reveal = None;
}

/// Prove the bounded safety behavior without treating it as a liveness fix:
/// the old 4-member set remains authoritative and produces blocks, the 5-member
/// target never partially activates, and progress stops only after VRF expiry.
#[then("the old committee finalizes without partial activation until VRF expiry")]
fn old_committee_reaches_expiry_without_partial_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let kill_height = world.state.marker_height.expect("kill height");
    let mut highest = kill_height;
    let mut saw_active_four = false;
    let mut expiry = None;

    for _ in 0..120 {
        if let Some(height) = world.rpc.finalized(primary) {
            highest = highest.max(height);
        }
        if let Some(active) = world.rpc.active_count(primary) {
            assert_eq!(active, 4, "frozen 4-to-5 target partially activated");
            saw_active_four = true;
        }
        if expiry.is_none() {
            expiry = world
                .rpc
                .consensus_status_field(primary, "vrfExpiryHeight")
                .and_then(|value| value.trim_matches('"').parse::<u64>().ok());
        }
        if (0..3).all(|index| {
            world
                .localnet
                .log_has(index, "frozen DKG target missed VRF expiry")
                .expect("read required owned process log")
        }) {
            break;
        }
        sleep(Duration::from_secs(2));
    }

    let expiry = expiry.expect("VRF expiry was never published while RPC was live");
    assert!(saw_active_four, "never observed the old active set");
    assert!(
        highest > kill_height + 6,
        "old committee did not continue finalizing before expiry"
    );
    assert!(
        highest >= expiry,
        "committee stopped before the published VRF expiry ({highest} < {expiry})"
    );
    world.state.vrf_expiry_height = Some(expiry);
}

#[then("the surviving validators exit with the frozen-target expiry error")]
fn surviving_validators_fail_closed(world: &mut World) {
    let expiry = world.state.vrf_expiry_height.expect("VRF expiry height");
    for index in 0..3 {
        assert!(
            world
                .localnet
                .log_has(index, "frozen DKG target missed VRF expiry")
                .expect("read required owned process log"),
            "validator-{index} lacks frozen-target expiry evidence (deadline {expiry})"
        );
        assert!(
            world.localnet.validator_exited(index),
            "validator-{index} remained running after frozen-target expiry"
        );
    }
}

/// The old committee keeps finalizing through the stalled reshare and the join
/// does not activate (s5:50-63).
#[then("the old committee keeps finalizing through the stalled reshare")]
fn old_committee_keeps_finalizing(world: &mut World) {
    let primary = world.validators.primary_port();
    let kill_h = world.state.marker_height.expect("kill height");

    let mut retry = 0usize;
    let mut alive_grow = false;
    for _ in 0..30 {
        sleep(Duration::from_secs(10));
        let Some(h) = world.rpc.head(primary) else {
            continue;
        };
        retry = world
            .localnet
            .log_count(0, "DKG reshare failed, retrying frozen target")
            .expect("read required owned process log");
        if h > kill_h + 12 {
            alive_grow = true;
        }
        if retry >= 1 && alive_grow {
            break;
        }
    }
    assert!(
        retry >= 1,
        "DKG join-reshare did not retry (flag-based per height)"
    );
    assert!(
        alive_grow,
        "old committee did not keep finalizing (3-of-4 quorum)"
    );
    assert_eq!(
        world
            .localnet
            .log_count(0, "hard halt")
            .expect("read required owned process log"),
        0,
        "unexpected hard-halt (no such model exists)"
    );
    assert_eq!(
        world.rpc.active_count(primary),
        Some(4),
        "join must not activate while its reshare is failing"
    );
}

/// Keep the failed target frozen until the chain has certified blocks after
/// the originally planned activation height. This proves the later boundary
/// cannot be mistaken for the regular epoch grid by a FullNode follower.
#[then("the old committee crosses the planned activation height without partial activation")]
fn old_committee_crosses_planned_activation(world: &mut World) {
    let primary = world.validators.primary_port();
    let planned = world
        .rpc
        .consensus_status_field(primary, "nextPlannedActivationHeight")
        .and_then(|value| value.trim_matches('"').parse::<u64>().ok())
        .expect("planned DKG activation height");
    assert!(
        world
            .rpc
            .wait_block(primary, planned.saturating_add(2), 90)
            .is_some(),
        "old committee did not finalize beyond planned activation {planned}"
    );
    assert_eq!(
        world.rpc.active_count(primary),
        Some(4),
        "frozen 4-to-5 target partially activated at the planned boundary"
    );
    world.state.activation_height = Some(planned);
}

/// Relaunch the downed validator (`localnet.restart` re-launches dead nodes),
/// leaving the joiner down (s5:65-71).
#[when("the downed validator is restored")]
fn restore_validator(world: &mut World) {
    world.localnet.restart().expect("restart committee");
}

/// With 4 online acking players again, a later retry completes and the set
/// reaches 5 (s5:72-75).
#[then("the reshare completes and the active set reaches 5")]
fn reshare_completes(world: &mut World) {
    let primary = world.validators.primary_port();
    assert!(
        world.rpc.wait_active_count(primary, 5, 40),
        "reshare did not complete after the participant was restored (set != 5)"
    );
    let revealed = world
        .state
        .expected_dkg_reveal
        .as_deref()
        .expect("expected offline participant reveal identity");
    assert!(
        world
            .localnet
            .log_has(0, revealed)
            .expect("read required owned process log"),
        "reshare completed without recording the expected offline participant reveal"
    );
}

#[then("the delayed reshare activates off-grid and the active set reaches 5")]
fn delayed_reshare_activates_off_grid(world: &mut World) {
    let primary = world.validators.primary_port();
    let planned = world
        .state
        .activation_height
        .expect("captured planned activation height");
    assert!(
        world.rpc.wait_active_count(primary, 5, 60),
        "delayed reshare did not activate after its participant returned"
    );
    let observed = world.rpc.head(primary).expect("post-activation head");
    assert!(
        observed > planned,
        "delayed reshare was not observed beyond its planned boundary: {observed} <= {planned}"
    );
}
