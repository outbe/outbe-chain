//! A scenario-owned tmpfs gives RocksDB a real ENOSPC while consensus remains live.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::Address;
use cucumber::{given, then, when};

use crate::features::common::{bootstrap_localnet, start_bootstrapped_localnet};
use crate::world::localnet::StartOpts;
use crate::world::World;

#[given("a four-validator bounded Tribute localnet with a scenario-owned RocksDB volume")]
fn localnet_with_projection_fault_volume(world: &mut World) {
    bootstrap_localnet(world, 6, &[]);
    let day = world
        .ocomp
        .prepare_cross_currency_tribute_fixture()
        .expect("prepare bounded Tribute fixture");
    world.state.wwd = Some(day.to_string());
    world
        .projection
        .mount_rocksdb_fault_volume(0)
        .expect("mount scenario-owned projection volume");
    start_bootstrapped_localnet(world, &StartOpts::with_voting_window(6));
}

#[when("validator 0 RocksDB runs out of space during an encrypted Tribute")]
fn exhaust_projection_during_tribute(world: &mut World) {
    let key = world.validators.get(0).evm_key().expect("operator key");
    super::l2_registration::ensure_tribute_offer_operator(world, &key);
    let day = world.state.wwd.clone().expect("offering day");
    let (pid, _) = world
        .localnet
        .live_validator_and_enclave_pids(0)
        .expect("fault target is live");
    let healthy_port = world.validators.http_port(1);
    let finalized_before = world
        .rpc
        .finalized(healthy_port)
        .expect("healthy validator has a finalized height before ENOSPC");
    world
        .projection
        .exhaust_rocksdb_fault_volume()
        .expect("fill scenario-owned RocksDB volume");
    let submitted = world
        .rpc
        .tribute_offer_for_creator(&key, &day, Address::repeat_byte(0x79));
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut retry_logged = false;
    let mut finality_during_fault = false;
    while Instant::now() < deadline {
        let log = world
            .localnet
            .node_launch_log(0, pid)
            .expect("read exact fault-target launch log");
        if log.contains("offchain storage projection write failed; retrying exact atomic batch") {
            retry_logged = true;
        }
        finality_during_fault |= world
            .rpc
            .finalized(healthy_port)
            .is_some_and(|height| height > finalized_before);
        if retry_logged && finality_during_fault {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    world
        .projection
        .restore_rocksdb_fault_volume()
        .expect("restore projection write capacity before recovery deadline");
    let tx = submitted.expect("submit encrypted Tribute during RocksDB outage");
    assert!(
        retry_logged,
        "validator-0 never observed the real RocksDB write failure"
    );
    assert!(
        finality_during_fault,
        "healthy committee did not finalize while validator-0 RocksDB was full"
    );
    world.state.tribute_tx_hash = Some(tx);
}

#[then("the finalized Tribute reaches the same durable projection on every validator")]
fn projection_recovers_without_divergence(world: &mut World) {
    let tx = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("fault-window Tribute");
    assert!(
        world.rpc.wait_successful_receipt(tx, 240),
        "Tribute did not finalize"
    );
    let height = world
        .rpc
        .receipt_block_number(tx, world.validators.primary_port())
        .expect("Tribute inclusion height");
    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, height, 90),
            "validator {port} did not finalize after projection write recovery"
        );
    }
    world
        .projection
        .wait_for_tribute_projection_on_nodes(tx, 120, 4)
        .expect("all RocksDB projections converge after ENOSPC");
}
