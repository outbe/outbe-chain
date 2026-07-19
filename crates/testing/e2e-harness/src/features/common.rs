//! Steps shared across scenarios: localnet setup, height gating, state-root
//! parity. These back the `Given`/`Then` lines that every flow reuses.
//!
//! Environment choices (validator count, TEE mode) come from the CLI, not the
//! feature text — the setup step reads them off the `World` handles, and the
//! requirements themselves are declared as tags (see [`crate::env`]).

use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then};

use crate::world::localnet::StartOpts;
use crate::world::World;

/// Localnet setup shared by every flow. The committee size and TEE mode come
/// from the environment (`--validators` / `--tee`, gated by the scenario's
/// `@min-validators-N` / `@tee` tags); the voting window is a step parameter
/// (lib.sh:106-139, update_operator_flow.sh:48-69).
#[given(expr = "a fresh localnet with a {int}-block voting window")]
fn fresh_localnet(world: &mut World, window: u64) {
    boot_localnet(world, window, &[]);
}

/// Lifecycle accounting needs a claim to mature during the live scenario. This
/// changes only the generated E2E genesis, never the production seed defaults.
#[given(expr = "a fresh lifecycle localnet with a {int}-block voting window")]
fn fresh_lifecycle_localnet(world: &mut World, window: u64) {
    boot_localnet(
        world,
        window,
        &[("TESTNET_UNBONDING_PERIOD_SECS", "8".to_string())],
    );
}

/// Shared localnet setup used by every flow: cleanup, bootstrap N (with optional
/// `TESTNET_*` tuning), start with the environment's TEE mode, and prove the
/// chain is up (TEE bootstrapped, or RPC reachable tee-less). Also captures the
/// chain's worldwide-day so tribute-offer steps target the OFFERING day.
pub(crate) fn boot_localnet(world: &mut World, window: u64, tuning: &[(&str, String)]) {
    boot_localnet_with_opts(world, window, tuning, StartOpts::with_voting_window(window));
}

pub(crate) fn boot_localnet_with_opts(
    world: &mut World,
    window: u64,
    tuning: &[(&str, String)],
    opts: StartOpts,
) {
    let committee_size = world.validators.size();
    world.state.voting_window = window;
    world.state.wwd = Some(crate::world::localnet::worldwide_day());
    // No pre-bootstrap reset: the scenario owns a fresh `scenario-<n>` dir and a
    // fresh block of ports, so there is nothing of its own to reclaim.
    world
        .localnet
        .bootstrap(committee_size, tuning)
        .expect("bootstrap localnet");
    if let Some(offset) = opts.unix_time_offset_secs {
        world
            .localnet
            .shift_genesis_timestamp(offset)
            .expect("shift debug genesis timestamp with node clock");
    }
    world.localnet.start(&opts).expect("start localnet");

    if world.localnet.tee_enabled() {
        assert!(
            world.rpc.wait_bootstrapped(18),
            "TEE chain did not bootstrap"
        );
    } else {
        // Tee-less: just prove the primary RPC is reachable (E2E_NO_TEE branch).
        let port = world.validators.primary_port();
        assert!(
            world.rpc.wait_block(port, 1, 18).is_some(),
            "tee-less chain RPC not reachable"
        );
    }
}

/// Wait for the committee to reach a usable height (>= 5), like the
/// `wait for RPC and a few blocks` step (update_operator_flow.sh:207-218).
#[given("the committee has reached a usable height")]
fn usable_height(world: &mut World) {
    let port = world.validators.primary_port();
    let h = world.rpc.wait_block(port, 5, 60).unwrap_or(0);
    assert!(h >= 5, "committee did not reach height 5 (got {h})");
}

/// State-root parity across the committee at a common finalized height
/// (update_operator_flow.sh:321-329). Iterates the actual committee size.
#[then("the committee nodes agree on the state root")]
fn state_root_parity(world: &mut World) {
    sleep(Duration::from_secs(6));
    let primary = world.validators.primary_port();
    let pn = world
        .rpc
        .finalized(primary)
        .or_else(|| world.rpc.head(primary))
        .expect("no usable height for parity");
    let sr0 = world
        .rpc
        .state_root(primary, pn)
        .expect("primary state root");
    for port in world.validators.peer_ports() {
        let sr = world.rpc.state_root(port, pn).unwrap_or_default();
        assert_eq!(sr, sr0, "state_root mismatch at h{pn} on port {port}");
    }
}
