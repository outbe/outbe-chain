//! Production snapshot startup and restart without computation-job prerequisites.

use std::{
    process::Command,
    time::{Duration, Instant},
};

use cucumber::when;
use eyre::{ensure, eyre, Result};

use super::offline_snapshot::{
    assert_local_committee_anchor, canonical_snapshot_block, parse_recovery_record,
    place_snapshot_payload, run_snapshot_command, snapshot_option, successful_command,
};
use crate::world::{localnet::NodeLaunchObservation, World};

mod upstream;
use upstream::ObservedUpstream;

const FOLLOWER: &str = "snapshot-recipient";
pub(super) const EPOCH_LENGTH: u64 = 60;

#[when("a signed native snapshot FullNode starts and restarts from its local committee anchor")]
fn snapshot_start_and_restart(world: &mut World) {
    if let Err(error) = run_snapshot_start_and_restart(world) {
        panic!("actual snapshot FullNode startup and restart: {error:#}");
    }
}

fn run_snapshot_start_and_restart(world: &mut World) -> Result<()> {
    let primary = world.validators.primary_port();
    let minimum_cut = EPOCH_LENGTH * 2;
    ensure!(
        world
            .rpc
            .wait_finalized_at_least(primary, minimum_cut + 10, 600),
        "committee did not rotate"
    );
    let slot = world.validators.joiner_index();
    world.localnet.prepare_snapshot_full_node(slot)?;
    world
        .ocomp
        .stage_keyless_full_node_domain(slot.try_into()?)?;
    let recipient = world.validators.data_dir(slot);
    ensure!(
        !recipient.join("db").exists(),
        "recipient already has execution history"
    );
    let chain_id = world
        .rpc
        .chain_id(primary)
        .ok_or_else(|| eyre!("chain id missing"))?;
    ensure!(chain_id == 54322345, "real SGX testnet profile required");
    let evidence = world
        .localnet
        .scenario_dir()
        .join("fullnode-snapshot-anchor");
    std::fs::create_dir_all(&evidence)?;
    let donor = world.localnet.validator_launch_observation(3)?;
    let _clients = world.ocomp.stop_node_facing_roles_for_snapshot(3)?;
    let _stopped = world.localnet.stop_validator_for_snapshot(3, donor.pid)?;
    let archive = evidence.join("snapshot.tar");
    let mut command = Command::new(&donor.program);
    command
        .args(["snapshot", "create", "--output"])
        .arg(&archive)
        .arg("--signing-key")
        .arg(world.validators.get(3).evm_key_path())
        .arg("--")
        .args(&donor.argv[1..]);
    let create = run_snapshot_command(command, &evidence, "create", Duration::from_secs(300))?;
    successful_command(&create)?;
    let (manifest, signature) = place_snapshot_payload(world, &archive, &evidence)?;
    std::fs::write(evidence.join("manifest.json"), &manifest)?;
    std::fs::write(evidence.join("signature.json"), signature)?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest)?;
    let cut = manifest["progress"]["finalized"]["number"]
        .as_u64()
        .ok_or_else(|| eyre!("snapshot cut missing"))?;
    ensure!(cut >= minimum_cut, "snapshot has no rotated committee");
    // Keep the donor offline; the live quorum is the source of new finality.

    let mut minimum_recovery = cut;
    for phase in ["imported", "restarted"] {
        let observer =
            ObservedUpstream::start(primary, &evidence.join(format!("{phase}-requests.jsonl")))?;
        world
            .localnet
            .launch_full_node_with_upstream_url(FOLLOWER, slot, 0, &observer.url())?;
        let launch = world.localnet.follower_launch_observation(FOLLOWER, slot)?;
        ensure!(
            snapshot_option(&launch.argv, "--upstream")? == observer.url(),
            "unobserved upstream"
        );
        let observed = observe_consensus_startup(world, &launch, minimum_recovery)?;
        let target = world
            .rpc
            .finalized(primary)
            .ok_or_else(|| eyre!("upstream finality missing"))?
            .checked_add(3)
            .ok_or_else(|| eyre!("height overflow"))?;
        ensure!(
            world
                .rpc
                .wait_finalized_at_least(world.validators.http_port(slot), target, 180),
            "FullNode did not advance after {phase}"
        );
        let snapshot_parity = assert_canonical_parity(world, slot, cut)?;
        let continued_parity = assert_canonical_parity(world, slot, target)?;
        std::fs::write(evidence.join(format!("{phase}-startup.log")), &observed.log)?;
        let stopped = world
            .localnet
            .stop_follower_for_snapshot(FOLLOWER, slot, launch.pid)?;
        let requests = observer.finish(observed.anchor)?;
        std::fs::write(
            evidence.join(format!("{phase}-result.json")),
            serde_json::to_vec_pretty(&serde_json::json!({"snapshot_cut":cut,"pid":launch.pid,
                "recovery_height":observed.height,
                "committee_anchor":observed.anchor,
                "canonical_parity_height":target,"snapshot_parity":snapshot_parity,
                "continued_parity":continued_parity,"argv":launch.argv,
                "consensus_requests":requests,
                "exit_code":stopped.observation.code,"exit_signal":stopped.observation.signal}))?,
        )?;
        minimum_recovery = target;
    }
    Ok(())
}

struct ConsensusStartup {
    log: String,
    height: u64,
    anchor: u64,
}

fn observe_consensus_startup(
    world: &mut World,
    launch: &NodeLaunchObservation,
    minimum_height: u64,
) -> Result<ConsensusStartup> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let log = loop {
        let log = world.localnet.node_launch_log(launch.index, launch.pid)?;
        if log.contains("certified follower startup recovery barrier completed") {
            break log;
        }
        ensure!(
            Instant::now() < deadline,
            "FullNode startup barrier missing: {}",
            launch.log_path.display()
        );
        world
            .localnet
            .follower_launch_observation(FOLLOWER, launch.index)?;
        std::thread::sleep(Duration::from_millis(100));
    };
    let (processed, checkpoint, ce, execution) = parse_recovery_record(&log)?;
    assert_local_committee_anchor(&log, checkpoint.number)?;
    ensure!(
        checkpoint.number >= minimum_height,
        "FullNode recovery regressed below {minimum_height}"
    );
    ensure!(
        checkpoint == canonical_snapshot_block(world, checkpoint.number)?,
        "noncanonical recovery checkpoint"
    );
    ensure!(
        processed <= checkpoint.number && checkpoint.number <= processed.saturating_add(1),
        "recovery outside processed frontier"
    );
    ensure!(
        ce >= checkpoint.number && execution >= checkpoint.number,
        "durable execution did not recover the consensus checkpoint"
    );
    let anchor = observed_anchor(&log)?;
    Ok(ConsensusStartup {
        log,
        height: checkpoint.number,
        anchor,
    })
}

fn observed_anchor(log: &str) -> Result<u64> {
    let record = log
        .lines()
        .find(|line| line.contains("follower restored committee from local finalized history"))
        .ok_or_else(|| eyre!("missing local committee anchor"))?;
    record
        .split_whitespace()
        .find_map(|part| part.strip_prefix("anchor_height="))
        .ok_or_else(|| eyre!("missing committee anchor height"))?
        .parse()
        .map_err(Into::into)
}

fn assert_canonical_parity(world: &World, slot: usize, height: u64) -> Result<serde_json::Value> {
    let primary = world.validators.primary_port();
    let follower = world.validators.http_port(slot);
    let expected_hash = world
        .rpc
        .block_hash(primary, height)
        .ok_or_else(|| eyre!("canonical hash missing"))?;
    let expected_root = world
        .rpc
        .state_root(primary, height)
        .ok_or_else(|| eyre!("canonical state root missing"))?;
    ensure!(
        world.rpc.block_hash(follower, height).as_ref() == Some(&expected_hash),
        "FullNode hash differs at {height}"
    );
    ensure!(
        world.rpc.state_root(follower, height).as_ref() == Some(&expected_root),
        "FullNode state root differs at {height}"
    );
    Ok(serde_json::json!({"height":height,"hash":expected_hash,"state_root":expected_root}))
}
