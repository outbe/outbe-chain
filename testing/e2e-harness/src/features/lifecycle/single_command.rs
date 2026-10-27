//! OIP-00337: one `--validator --upstream` command takes a joiner through
//! `upstreamBootstrap -> verifier -> signer` in one process, and a restart with
//! the same command boots straight into `signer`.
use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};
use eyre::{ensure, eyre, Result};
use serde_json::json;

use crate::world::World;

const MODE_POLL: Duration = Duration::from_millis(250);

/// Modes in the order that one validator process may report them. The RPC
/// reports `follower` before the consensus thread publishes a role.
const LIFECYCLE_ORDER: [&str; 4] = ["follower", "upstreamBootstrap", "verifier", "signer"];

/// True when the distinct `outbe_consensusStatus.mode` values only move
/// forward in the lifecycle order.
pub(super) fn mode_progression_is_monotonic(observed: &[String]) -> bool {
    observed
        .iter()
        .map(|mode| LIFECYCLE_ORDER.iter().position(|known| known == mode))
        .try_fold(0, |previous, rank| match rank {
            Some(rank) if rank >= previous => Some(rank),
            _ => None,
        })
        .is_some()
}

fn record_mode(observed: &mut Vec<String>, mode: String) {
    if observed.last() != Some(&mode) {
        observed.push(mode);
    }
}

/// Poll the joiner until it reports `target`, and keep every distinct mode.
fn wait_for_mode(world: &World, target: &str, seconds: u64) -> Result<Vec<String>> {
    let port = world.validators.http_port(world.validators.joiner_index());
    let mut observed = Vec::new();
    let polls = seconds.saturating_mul(1_000) / MODE_POLL.as_millis() as u64;
    for _ in 0..polls.max(1) {
        if let Some(mode) = world.rpc.consensus_status_field(port, "mode") {
            let reached = mode == target;
            record_mode(&mut observed, mode);
            if reached {
                return Ok(observed);
            }
        }
        sleep(MODE_POLL);
    }
    Err(eyre!(
        "joiner did not report mode {target} within {seconds}s (observed {observed:?})"
    ))
}

fn same_process(world: &mut World) -> Result<(u32, u32)> {
    let index = world.validators.joiner_index();
    let pids = world.localnet.live_validator_and_enclave_pids(index)?;
    let expected = world
        .state
        .single_command_pids
        .ok_or_else(|| eyre!("missing single-command process identity"))?;
    ensure!(
        pids == expected,
        "lifecycle replaced a process: {expected:?} -> {pids:?}"
    );
    Ok(pids)
}

#[when("a registered joiner starts once with --validator and --upstream")]
fn joiner_starts_with_one_command(world: &mut World) {
    (|| -> Result<()> {
        let index = world.validators.joiner_index();
        world.localnet.provision_joiner(index)?;
        let keys = world.localnet.keys_dir(index);
        world
            .localnet
            .launch_joiner(index, &["--consensus.keys-dir", &keys])?;
        let pids = world.localnet.live_validator_and_enclave_pids(index)?;
        world.state.single_command_pids = Some(pids);
        let key = world.validators.joiner().evm_key()?;
        world.state.joiner_addr = Some(
            world
                .rpc
                .address_of(&key)
                .ok_or_else(|| eyre!("joiner identity"))?,
        );
        Ok(())
    })()
    .expect("registered joiner launched with one validator command");
}

#[then("it reports upstreamBootstrap and then verifier in the same node and enclave processes")]
fn joiner_bootstraps_then_verifies(world: &mut World) {
    (|| -> Result<()> {
        let bootstrap = wait_for_mode(world, "upstreamBootstrap", 60)?;
        let verifier = wait_for_mode(world, "verifier", 240)?;
        let observed: Vec<String> = bootstrap.into_iter().chain(verifier).collect();
        ensure!(
            mode_progression_is_monotonic(&observed),
            "mode moved backwards: {observed:?}"
        );
        let index = world.validators.joiner_index();
        ensure!(
            world
                .rpc
                .has_threshold_shares(world.validators.http_port(index))
                != Some(true),
            "a verifier must not hold a threshold share"
        );
        let (node_pid, enclave_pid) = same_process(world)?;
        world.state.restart_observations.push(json!({
            "phase": "single_command_verifier", "modes": observed,
            "node_pid": node_pid, "enclave_pid": enclave_pid,
        }));
        Ok(())
    })()
    .expect("UpstreamBootstrap promoted to Verifier without a restart");
}

#[when("the single-command joiner tops up and confirms readiness")]
fn single_command_joiner_confirms(world: &mut World) {
    (|| -> Result<()> {
        let key = world.validators.joiner().evm_key()?;
        world.rpc.topup(&key, 1000)?;
        sleep(Duration::from_secs(6));
        world.rpc.confirm_ready(&key)?;
        Ok(())
    })()
    .expect("joiner stake and readiness submitted");
}

#[then("it reports signer in the same node and enclave processes and signs blocks")]
fn joiner_becomes_signer(world: &mut World) {
    (|| -> Result<()> {
        let observed = wait_for_mode(world, "signer", 300)?;
        ensure!(
            mode_progression_is_monotonic(&observed)
                && !observed.iter().any(|mode| mode == "upstreamBootstrap"),
            "unexpected modes after verifier: {observed:?}"
        );
        let addr = world
            .state
            .joiner_addr
            .clone()
            .ok_or_else(|| eyre!("missing joiner identity"))?;
        ensure!(
            world
                .rpc
                .wait_participant(world.validators.primary_port(), &addr, 70)?,
            "signer did not become a consensus participant"
        );
        let index = world.validators.joiner_index();
        ensure!(
            world
                .rpc
                .has_threshold_shares(world.validators.http_port(index))
                == Some(true),
            "signer has no threshold share"
        );
        ensure!(
            world.localnet.has_share_file(index),
            "share was not persisted"
        );
        let (node_pid, enclave_pid) = same_process(world)?;
        world.state.restart_observations.push(json!({
            "phase": "single_command_signer", "modes": observed,
            "node_pid": node_pid, "enclave_pid": enclave_pid,
        }));
        Ok(())
    })()
    .expect("Verifier promoted to Signer at the DKG boundary without a restart");
}

#[when("the joiner restarts with the same validator command")]
fn joiner_restarts_with_same_command(world: &mut World) {
    (|| -> Result<()> {
        let index = world.validators.joiner_index();
        let argv = world
            .localnet
            .validator_argv(index)
            .ok_or_else(|| eyre!("missing joiner argv"))?;
        world.localnet.stop_joiner(index)?;
        let keys = world.localnet.keys_dir(index);
        world
            .localnet
            .launch_joiner(index, &["--consensus.keys-dir", &keys])?;
        ensure!(
            world.localnet.validator_argv(index) == Some(argv),
            "restart did not reuse the exact validator command"
        );
        Ok(())
    })()
    .expect("joiner restarted with the same command");
}

#[then("it boots straight into signer without UpstreamBootstrap")]
fn joiner_boots_into_signer(world: &mut World) {
    (|| -> Result<()> {
        let observed = wait_for_mode(world, "signer", 120)?;
        ensure!(
            observed
                .iter()
                .all(|mode| mode == "follower" || mode == "signer"),
            "restart passed through another phase: {observed:?}"
        );
        world.state.restart_observations.push(json!({
            "phase": "single_command_restart", "modes": observed,
        }));
        Ok(())
    })()
    .expect("bootstrap-ready restart skips UpstreamBootstrap");
}
