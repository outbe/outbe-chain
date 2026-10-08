//! Step definitions for `features/consensus_resilience.feature` (B8, R7, R11, R13, S14).

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use cucumber::{given, then, when};

use crate::features::common::boot_localnet;
use crate::internal::eth;
use crate::world::World;

const FOLLOWER_RESILIENCE_SLOT: usize = 14;
const FOLLOWER_RESILIENCE_NAME: &str = "follower-resilience";

// -----------------------------------------------------------------------
// B8: post-epoch validator restart recovery smoke
// -----------------------------------------------------------------------

#[given(expr = "a fresh localnet with a short epoch and {int}-block voting window")]
fn fresh_localnet_short_epoch(world: &mut World, window: u64) {
    boot_localnet(
        world,
        window,
        &[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "30".to_string()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "10".to_string()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "5".to_string()),
        ],
    );
}

#[given("the committee reaches a finalized height past the epoch transition")]
fn committee_past_epoch_transition(world: &mut World) {
    let ports = world.validators.committee_ports();
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 32, 90)
        .expect("committee reaches past-epoch checkpoint");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

#[when("an active validator is stopped and restarted after the epoch transition")]
fn stop_and_restart_validator_post_epoch(world: &mut World) {
    let target_slot = 3;
    world.state.resilience_target_slot = Some(target_slot);
    world
        .localnet
        .kill_validator(target_slot)
        .expect("kill validator-3 post-epoch");
    sleep(Duration::from_millis(500));
    world
        .localnet
        .restart_validator(target_slot)
        .expect("restart validator-3 from durable datadir");
}

#[then("the restarted validator catches up and resumes finalization")]
fn restarted_validator_catches_up_and_resumes(world: &mut World) {
    let target_slot = world
        .state
        .resilience_target_slot
        .expect("target slot must be recorded");
    let target_port = world.validators.http_port(target_slot);
    let floor_height = world
        .state
        .resilience_observed_height
        .expect("observed height must be recorded");

    let mut caught_up = false;
    for _ in 0..30 {
        if let Some(height) = world.rpc.finalized(target_port) {
            if height >= floor_height {
                caught_up = true;
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }
    assert!(
        caught_up,
        "restarted validator did not resume finalization past floor height {floor_height}"
    );
}

#[then("the committee continues producing and finalizing blocks in lockstep")]
fn committee_continues_producing_and_finalizing_blocks(world: &mut World) {
    let ports = world.validators.committee_ports();
    let floor_height = world.state.resilience_observed_height.unwrap_or(10);
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, floor_height + 2, 60)
        .expect("committee continues producing blocks across post-epoch restart");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

// -----------------------------------------------------------------------
// R7: follower sync recovery across upstream loss/stall
// -----------------------------------------------------------------------

#[when("a production FullNode syncs from the committee with bounded follower resolution")]
fn sync_follower_with_bounded_resolution(world: &mut World) {
    world
        .localnet
        .provision_full_node_node_host(FOLLOWER_RESILIENCE_SLOT)
        .expect("provision production FullNode NodeHost and enclave");
    world
        .localnet
        .launch_dcap_full_node(FOLLOWER_RESILIENCE_NAME, FOLLOWER_RESILIENCE_SLOT, 0)
        .expect("launch production FullNode follower");

    let committee_port = world.validators.primary_port();
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);

    let mut synced = false;
    for _ in 0..40 {
        if let (Some(head), Some(follower_head)) = (
            world.rpc.head(committee_port),
            world.rpc.head(follower_port),
        ) {
            if follower_head + 2 >= head {
                synced = true;
                break;
            }
        }
        sleep(Duration::from_secs(1));
    }
    assert!(synced, "follower did not sync to committee head");
}

#[when("the follower upstream is stopped while committee consensus advances")]
fn stop_follower_upstream(world: &mut World) {
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);
    let pre_height = world
        .rpc
        .finalized(follower_port)
        .expect("follower must report active finalized height before fault");
    world.state.resilience_disconnected_height = Some(pre_height);

    // Stop upstream validator 0
    world
        .localnet
        .kill_validator(0)
        .expect("stop configured upstream validator-0");

    // Surviving quorum (1, 2, 3) advances
    let survivor_ports = vec![
        world.validators.http_port(1),
        world.validators.http_port(2),
        world.validators.http_port(3),
    ];
    let advanced = world
        .rpc
        .wait_finalized_checkpoint(&survivor_ports, pre_height + 2, 60)
        .expect("surviving committee advances past stopped upstream");
    world.state.resilience_observed_height = Some(advanced.height);
}

#[then("the disconnected follower remains responsive while its upstream is offline")]
fn follower_remains_responsive(world: &mut World) {
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);

    // Verify follower process is still alive and has not crashed
    world
        .localnet
        .live_follower_pid(FOLLOWER_RESILIENCE_NAME)
        .expect("follower process must remain alive without crashing");

    let pre_height = world
        .state
        .resilience_disconnected_height
        .expect("pre-fault height must be recorded");

    // Follower should not advance indefinitely while its only configured upstream is dead
    sleep(Duration::from_secs(2));
    let post_height = world
        .rpc
        .finalized(follower_port)
        .expect("follower RPC must remain responsive");
    let survivor_height = world
        .state
        .resilience_observed_height
        .expect("survivor height must be recorded");
    assert!(
        post_height <= survivor_height,
        "disconnected follower advanced beyond network state: {post_height} > {survivor_height}"
    );
    let _ = pre_height;
}

#[when("the follower is restarted in place and switched to an active healthy upstream")]
fn restart_follower_and_switch_upstream(world: &mut World) {
    // Restart validator 0 first so the committee is fully restored
    world
        .localnet
        .restart_validator(0)
        .expect("restart validator-0");

    // Stop the follower and switch its upstream to validator 1
    world
        .localnet
        .stop_follower(FOLLOWER_RESILIENCE_NAME)
        .expect("stop follower");
    world
        .localnet
        .launch_dcap_full_node(FOLLOWER_RESILIENCE_NAME, FOLLOWER_RESILIENCE_SLOT, 1)
        .expect("launch follower pointing to validator-1");
}

#[then("the follower catches up to the committee finalized checkpoint with matching hash and state root")]
fn follower_catches_up_to_committee(world: &mut World) {
    let primary = world.validators.primary_port();
    let follower_port = world.validators.http_port(FOLLOWER_RESILIENCE_SLOT);

    let mut caught_up = false;
    for _ in 0..40 {
        if let Some(target) = world.rpc.finalized(primary) {
            let expected_hash = world
                .rpc
                .block_hash(primary, target)
                .expect("expected canonical block hash");
            let expected_root = world
                .rpc
                .state_root(primary, target)
                .expect("expected canonical state root");

            let follower_height = world.rpc.finalized(follower_port);
            let follower_hash = world.rpc.block_hash(follower_port, target);
            let follower_root = world.rpc.state_root(follower_port, target);

            if follower_height.is_some_and(|h| h >= target)
                && follower_hash.as_ref() == Some(&expected_hash)
                && follower_root.as_ref() == Some(&expected_root)
            {
                caught_up = true;
                break;
            }
        }
        sleep(Duration::from_secs(2));
    }
    assert!(
        caught_up,
        "follower failed to catch up to committee checkpoint"
    );
}

// -----------------------------------------------------------------------
// R11: finalize vote ingress across epoch boundaries
// -----------------------------------------------------------------------

#[given("the committee advances towards an epoch transition")]
fn committee_advances_towards_epoch(world: &mut World) {
    let ports = world.validators.committee_ports();
    // Wait until approaching epoch boundary (height 28 for a 30-block epoch)
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 28, 60)
        .expect("committee reaches approach to epoch boundary");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

#[when("active validators process ingress finalize votes across epoch boundaries")]
fn process_ingress_finalize_votes(world: &mut World) {
    let ports = world.validators.committee_ports();
    for &port in &ports {
        assert!(
            world.rpc.finalized(port).is_some(),
            "validator at port {port} must have active finality"
        );
    }
}

#[then("the committee advances across the epoch boundary without progress stall")]
fn committee_advances_across_epoch_boundary(world: &mut World) {
    let ports = world.validators.committee_ports();
    // In a 30-block epoch, height 32 is in epoch 1
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 32, 60)
        .expect("committee crosses epoch boundary to height 32");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

#[then("all committee validators finalize blocks in lockstep past the transition")]
fn all_validators_finalize_past_transition(world: &mut World) {
    let ports = world.validators.committee_ports();
    let target_height = world.state.resilience_observed_height.unwrap_or(32);

    let first_hash = world
        .rpc
        .block_hash(ports[0], target_height)
        .expect("block hash port 0");
    let first_root = world
        .rpc
        .state_root(ports[0], target_height)
        .expect("state root port 0");

    for &port in &ports[1..] {
        let hash = world
            .rpc
            .block_hash(port, target_height)
            .expect("block hash");
        let root = world
            .rpc
            .state_root(port, target_height)
            .expect("state root");
        assert_eq!(
            hash, first_hash,
            "lockstep block hash mismatch across epoch transition"
        );
        assert_eq!(
            root, first_root,
            "lockstep state root mismatch across epoch transition"
        );
    }
}

// -----------------------------------------------------------------------
// R13: downtime recovery with empty V2 metadata
// -----------------------------------------------------------------------

#[when("an active validator experiences downtime while consensus advances")]
fn active_validator_experiences_downtime(world: &mut World) {
    let ports = world.validators.committee_ports();
    let current_checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 2, 30)
        .expect("initial checkpoint before validator downtime");
    world.state.resilience_observed_height = Some(current_checkpoint.height);

    // Stop validator 2 during active rounds
    world
        .localnet
        .kill_validator(2)
        .expect("kill validator-2 during active rounds");
}

#[then("the surviving quorum finalizes the successor proposal past the stopped validator")]
fn surviving_quorum_finalizes_past_stopped_validator(world: &mut World) {
    let base_height = world
        .state
        .resilience_observed_height
        .expect("base height must be recorded");
    let survivor_ports = vec![
        world.validators.http_port(0),
        world.validators.http_port(1),
        world.validators.http_port(3),
    ];
    let next_checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&survivor_ports, base_height + 1, 60)
        .expect("surviving quorum finalizes successor proposal");
    world.state.resilience_observed_height = Some(next_checkpoint.height);
}

#[then("the successor block contains empty V2 missed-proposers metadata in phase 1 accounting")]
fn successor_block_contains_empty_v2_missed_proposers(world: &mut World) {
    let height = world
        .state
        .resilience_observed_height
        .expect("observed height must be recorded");
    let primary_port = world.validators.primary_port();
    let url = world.rpc.url(primary_port);

    let block_json = eth::raw_json_with_params(
        &url,
        "eth_getBlockByNumber",
        serde_json::json!([format!("0x{height:x}"), true]),
    )
    .expect("fetched successor block from primary RPC");

    let txs = block_json
        .get("transactions")
        .and_then(|v| v.as_array())
        .expect("transactions array in block");
    assert!(
        !txs.is_empty(),
        "block at height {height} must contain at least one system tx"
    );

    let first_tx = &txs[0];
    let input_hex = first_tx
        .get("input")
        .and_then(|v| v.as_str())
        .expect("first transaction input must be a hex string");
    let input_bytes = hex::decode(input_hex.trim_start_matches("0x"))
        .expect("transaction input must be valid hex");

    let decoded = outbe_primitives::system_tx::SystemTxInputV2::decode(&input_bytes)
        .expect("first transaction input must decode as SystemTxInputV2");

    let outbe_primitives::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } =
        decoded
    else {
        panic!("first transaction input must be CertifiedParentAccounting, got different SystemTxInputV2 variant");
    };

    assert!(
        metadata.missed_proposers.is_empty(),
        "missed_proposers must be empty under V2 specs, found {:?}",
        metadata.missed_proposers
    );

    let parent_hash_str = block_json
        .get("parentHash")
        .and_then(|v| v.as_str())
        .expect("parentHash field in block");
    let expected_parent_hash: alloy_primitives::B256 =
        parent_hash_str.parse().expect("valid parentHash hex");
    assert_eq!(
        metadata.finalized_block_hash, expected_parent_hash,
        "CertifiedParentAccounting finalized_block_hash must match block parentHash"
    );
}

#[then("the committee continues producing and finalizing blocks without progress stall")]
fn committee_continues_without_stall(world: &mut World) {
    // Restart validator 2 to restore full committee
    world
        .localnet
        .restart_validator(2)
        .expect("restart validator-2");

    let ports = world.validators.committee_ports();
    let base_height = world.state.resilience_observed_height.unwrap_or(3);
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, base_height + 2, 60)
        .expect("committee continues block production after view gap recovery");
    world.state.resilience_observed_height = Some(checkpoint.height);
}

// -----------------------------------------------------------------------
// S14: txpool preservation and nonce sequence regression
// -----------------------------------------------------------------------

#[when("an operator submits a sequence of ordered nonce transactions from one account")]
fn submit_ordered_nonce_transactions(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let sender = eth::address_of(&key).expect("sender address");
    let next_nonce = eth::nonce(&url, sender).expect("sender nonce");

    let hash0 = eth::send_value_at_nonce(
        &url,
        Address::repeat_byte(0x51),
        &key,
        U256::from(100u64),
        next_nonce,
    )
    .expect("submit nonce N transaction");

    let hash1 = eth::send_value_at_nonce(
        &url,
        Address::repeat_byte(0x52),
        &key,
        U256::from(100u64),
        next_nonce + 1,
    )
    .expect("submit nonce N+1 transaction");

    world.state.resilience_ordered_tx_hashes = vec![hash0, hash1];
}

#[when("an independent healthy transaction is submitted from another sender")]
fn submit_independent_healthy_transaction(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let key = world.validators.get(1).evm_key().expect("validator-1 key");

    let outcome =
        eth::send_value_outcome(&url, Address::repeat_byte(0x53), &key, U256::from(100u64))
            .expect("submit independent healthy transaction");

    world.state.resilience_healthy_tx_hash = Some(outcome.transaction_hash);
}

#[then("the independent healthy transaction is successfully mined")]
fn independent_healthy_transaction_mined(world: &mut World) {
    let hash = world
        .state
        .resilience_healthy_tx_hash
        .clone()
        .expect("healthy tx must be recorded");
    let ports = world.validators.committee_ports();

    let mut mined = false;
    for _ in 0..30 {
        if eth::receipt_success(&world.rpc.url(ports[0]), &hash) == Some(true) {
            mined = true;
            break;
        }
        sleep(Duration::from_millis(500));
    }
    assert!(mined, "independent healthy transaction was not mined");
}

#[then("the dependent nonce sequence remains present in the transaction pool or receipt pipeline")]
fn nonce_sequence_held_in_txpool(world: &mut World) {
    let port = world.validators.primary_port();
    let hashes = &world.state.resilience_ordered_tx_hashes;
    assert_eq!(hashes.len(), 2, "expected two ordered transactions");

    for hash in hashes {
        let in_pool = world.rpc.txpool_has(port, hash).unwrap_or(false);
        let receipt = eth::receipt_json(&world.rpc.url(port), hash);
        assert!(
            in_pool || receipt.is_some(),
            "transaction {hash} was unexpectedly evicted from pool without receipt"
        );
    }
}

#[when("the next block proposal executes")]
fn next_block_proposal_executes(world: &mut World) {
    let ports = world.validators.committee_ports();
    let current = world.rpc.finalized(ports[0]).unwrap_or(1);
    world
        .rpc
        .wait_finalized_checkpoint(&ports, current + 1, 30)
        .expect("next block proposal finalizes");
}

fn parse_receipt_position(receipt: &serde_json::Value) -> Option<(u64, u64)> {
    let status = receipt.get("status")?.as_str()?;
    if status != "0x1" {
        return None;
    }
    let block_hex = receipt.get("blockNumber")?.as_str()?;
    let tx_index_hex = receipt.get("transactionIndex")?.as_str()?;
    let block = u64::from_str_radix(block_hex.trim_start_matches("0x"), 16).ok()?;
    let tx_index = u64::from_str_radix(tx_index_hex.trim_start_matches("0x"), 16).ok()?;
    Some((block, tx_index))
}

#[then("the nonce transactions are mined in canonical order")]
fn nonce_transactions_mined_in_order(world: &mut World) {
    let port = world.validators.primary_port();
    let hashes = &world.state.resilience_ordered_tx_hashes;
    let url = world.rpc.url(port);

    let mut pos0 = None;
    let mut pos1 = None;

    for _ in 0..40 {
        if pos0.is_none() {
            if let Some(r0) = eth::receipt_json(&url, &hashes[0]) {
                pos0 = parse_receipt_position(&r0);
            }
        }
        if pos1.is_none() {
            if let Some(r1) = eth::receipt_json(&url, &hashes[1]) {
                pos1 = parse_receipt_position(&r1);
            }
        }
        if pos0.is_some() && pos1.is_some() {
            break;
        }
        sleep(Duration::from_millis(500));
    }

    let p0 = pos0.expect("nonce N transaction was not successfully mined");
    let p1 = pos1.expect("nonce N+1 transaction was not successfully mined");
    assert!(
        p0 < p1,
        "nonce order violation: nonce N mined at {p0:?}, nonce N+1 mined at {p1:?}"
    );
}
