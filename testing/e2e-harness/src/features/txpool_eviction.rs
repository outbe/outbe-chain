//! Steps for `features/txpool_eviction.feature` - pool lifetime bounds.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use cucumber::{then, when};

use crate::internal::eth;
use crate::world::World;

/// Nonce distance that guarantees the transaction can never become executable
/// during the scenario: the account would have to send this many transactions
/// first.
const UNREACHABLE_NONCE_GAP: u64 = 64;

/// The harness runs validators with `--txpool.lifetime 30s`; allow generous
/// margin for the maintenance tick that performs the eviction.
const LIFETIME_WAIT: Duration = Duration::from_secs(75);
const PENDING_STALENESS_WAIT: Duration = Duration::from_secs(50);
const TXPOOL_FOLLOWER_SLOT: usize = 14;
const TXPOOL_FOLLOWER_NAME: &str = "txpool-follower";

#[when("an operator submits a transaction with an unreachable nonce")]
fn submit_unreachable_nonce_tx(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let sender = eth::address_of(&key).expect("sender address");
    let next_nonce = eth::nonce(&url, sender).expect("sender nonce");
    let unreachable_nonce = next_nonce + UNREACHABLE_NONCE_GAP;
    let hash = eth::send_value_at_nonce(
        &url,
        Address::repeat_byte(0x77),
        &key,
        U256::from(1u64),
        unreachable_nonce,
    )
    .expect("submit an unreachable-nonce transaction");
    world.state.stuck_tx_hash = Some(hash);
    world.state.stuck_tx_sender = Some(format!("{sender}"));
    world.state.stuck_tx_nonce = Some(unreachable_nonce);
}

#[then("the unreachable transaction sits in the pool")]
fn unreachable_tx_is_pooled(world: &mut World) {
    let port = world.validators.primary_port();
    let hash = world
        .state
        .stuck_tx_hash
        .clone()
        .expect("an unreachable transaction was submitted");
    for _ in 0..20 {
        if world
            .rpc
            .txpool_has(port, &hash)
            .expect("observe queued transaction in txpool")
        {
            return;
        }
        sleep(Duration::from_millis(500));
    }
    panic!("the unreachable transaction never appeared in the pool: {hash}");
}

#[then("an ordinary transfer submitted alongside is mined")]
fn ordinary_transfer_is_mined(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    // `send_value` waits for the receipt, so returning at all proves the chain
    // keeps mining while the unreachable transaction is parked.
    let hash = eth::send_value(&url, Address::repeat_byte(0x78), &key, U256::from(1u64))
        .expect("ordinary transfer must still be mined");
    assert!(
        !world
            .rpc
            .txpool_has(port, &hash)
            .expect("observe evicted transaction in txpool"),
        "a mined transfer must not remain in the pool"
    );
}

#[when("the pool lifetime elapses")]
fn wait_pool_lifetime(_world: &mut World) {
    sleep(LIFETIME_WAIT);
}

#[then("the unreachable transaction is gone from every validator's pool")]
fn unreachable_tx_evicted_everywhere(world: &mut World) {
    let hash = world
        .state
        .stuck_tx_hash
        .clone()
        .expect("an unreachable transaction was submitted");
    for index in 0..world.validators.size() {
        let port = world.validators.http_port(index);
        assert!(
            !world
                .rpc
                .txpool_has(port, &hash)
                .expect("observe transaction absence after eviction"),
            "validator-{index} still holds the unreachable transaction {hash}"
        );
    }
}

#[then("the submitting validator logged the exact eviction identity and reason")]
fn eviction_was_logged(world: &mut World) {
    // Evictions must never be silent. Only the submitting node is known to have
    // held this local RPC transaction, so bind its runtime record to the exact
    // hash, sender, nonce, and removal reason.
    let hash = world
        .state
        .stuck_tx_hash
        .as_deref()
        .expect("an unreachable transaction was submitted");
    let sender = world
        .state
        .stuck_tx_sender
        .as_deref()
        .expect("the unreachable transaction sender was captured");
    let nonce = world
        .state
        .stuck_tx_nonce
        .expect("the unreachable transaction nonce was captured");
    let line = world
        .localnet
        .first_runtime_log_line_containing(&format!("tx_hash={hash}"))
        .expect("scan validator runtime logs")
        .unwrap_or_else(|| panic!("validator-0 emitted no eviction record for {hash}"));
    assert!(
        line.contains("/validator-0/")
            && line.contains("outbe::txpool")
            && line.contains("evicting queued transaction after lifetime deadline")
            && line.contains(&format!("sender={sender}"))
            && line.contains(&format!("nonce={nonce}"))
            && line.contains("reason=\"queued_lifetime\""),
        "validator-0 eviction record did not bind the exact identity and reason: {line}"
    );
}

#[when("the submitting validator restarts after the queued eviction")]
fn restart_submitting_validator_after_eviction(world: &mut World) {
    world.state.marker_height = Some(
        world
            .rpc
            .finalized_result(world.validators.primary_port())
            .expect("capture finality before validator restart"),
    );
    world
        .localnet
        .restart_validator_preserving_enclave(0)
        .expect("restart the submitting validator with its durable node state");
}

#[then("the evicted transaction stays absent and the restarted committee finalizes")]
fn queued_eviction_survives_restart(world: &mut World) {
    let hash = world
        .state
        .stuck_tx_hash
        .as_deref()
        .expect("queued transaction hash");
    let target = world
        .state
        .marker_height
        .expect("pre-restart finalized height")
        .saturating_add(2);
    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, target, 60),
            "validator on port {port} did not finalize after txpool-owner restart"
        );
        assert_eq!(
            world
                .rpc
                .txpool_location(port, hash)
                .unwrap_or_else(|error| panic!("read txpool after restart: {error:#}")),
            None,
            "evicted queued transaction reappeared after validator restart"
        );
    }
}

#[when("an isolated production FullNode syncs from the committee for pending-pool testing")]
fn launch_isolated_txpool_follower(world: &mut World) {
    world
        .localnet
        .provision_full_node_node_host(TXPOOL_FOLLOWER_SLOT)
        .expect("provision isolated production FullNode");
    world
        .localnet
        .launch_isolated_txpool_follower(TXPOOL_FOLLOWER_NAME, TXPOOL_FOLLOWER_SLOT, 0)
        .expect("launch isolated production FullNode");
    let primary = world.validators.primary_port();
    let follower = world.validators.http_port(TXPOOL_FOLLOWER_SLOT);
    let target = world
        .rpc
        .finalized_result(primary)
        .expect("committee finalized height before follower catch-up");
    assert!(
        world.rpc.wait_finalized_at_least(follower, target, 90),
        "isolated FullNode did not catch up to committee finality"
    );
    assert_eq!(
        world
            .rpc
            .checkpoint_at(follower, target)
            .expect("follower checkpoint"),
        world
            .rpc
            .checkpoint_at(primary, target)
            .expect("committee checkpoint")
    );
}

#[when("an operator submits one executable transaction only to the isolated FullNode")]
fn submit_pending_to_isolated_follower(world: &mut World) {
    let port = world.validators.http_port(TXPOOL_FOLLOWER_SLOT);
    let url = world.rpc.url(port);
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let sender = eth::address_of(&key).expect("sender address");
    let nonce = eth::nonce(&url, sender).expect("follower canonical sender nonce");
    let hash = eth::send_value_at_nonce(
        &url,
        Address::repeat_byte(0x79),
        &key,
        U256::from(1u64),
        nonce,
    )
    .expect("submit executable transaction to isolated FullNode");
    world.state.stuck_tx_hash = Some(hash);
    world.state.stuck_tx_sender = Some(format!("{sender}"));
    world.state.stuck_tx_nonce = Some(nonce);
}

#[then("the executable transaction remains pending through the first snapshot")]
fn transaction_remains_pending_through_first_snapshot(world: &mut World) {
    let port = world.validators.http_port(TXPOOL_FOLLOWER_SLOT);
    let hash = world.state.stuck_tx_hash.as_deref().expect("pending hash");
    for _ in 0..20 {
        if world
            .rpc
            .txpool_location(port, hash)
            .expect("observe isolated follower pending pool")
            == Some("pending")
        {
            sleep(Duration::from_secs(10));
            assert_eq!(
                world
                    .rpc
                    .txpool_location(port, hash)
                    .expect("observe pending transaction after first snapshot"),
                Some("pending"),
                "pending transaction was removed before one full staleness interval"
            );
            return;
        }
        sleep(Duration::from_millis(250));
    }
    panic!("executable transaction never entered the pending sub-pool: {hash}");
}

#[when("the two-snapshot pending staleness window elapses")]
fn wait_pending_staleness_window(_world: &mut World) {
    sleep(PENDING_STALENESS_WAIT);
}

#[then("the FullNode evicts the exact pending transaction as stale")]
fn pending_transaction_evicted(world: &mut World) {
    let port = world.validators.http_port(TXPOOL_FOLLOWER_SLOT);
    let hash = world.state.stuck_tx_hash.as_deref().expect("pending hash");
    assert_eq!(
        world
            .rpc
            .txpool_location(port, hash)
            .expect("observe isolated follower after pending staleness window"),
        None,
        "stale executable transaction remained in the FullNode pending pool"
    );
    let line = world
        .localnet
        .first_runtime_log_line_containing(&format!("tx_hash={hash}"))
        .expect("scan required FullNode runtime log")
        .unwrap_or_else(|| panic!("FullNode emitted no pending-staleness eviction for {hash}"));
    assert!(
        line.contains("/validator-14/")
            && line.contains("evicting stale pending transaction")
            && line.contains("reason=\"stale_pending\"")
            && line.contains("staleness_interval_secs=20"),
        "pending eviction record did not bind the exact FullNode transaction and reason: {line}"
    );
}

#[then("the same nonce remains usable on the committee and finality continues")]
fn evicted_pending_nonce_remains_canonical(world: &mut World) {
    let key = world.validators.get(0).evm_key().expect("validator-0 key");
    let expected_nonce = world.state.stuck_tx_nonce.expect("pending nonce");
    let primary = world.validators.primary_port();
    assert_eq!(
        eth::nonce(
            &world.rpc.url(primary),
            eth::address_of(&key).expect("sender")
        )
        .expect("committee canonical nonce"),
        expected_nonce,
        "isolated pending transaction unexpectedly changed canonical account state"
    );
    let hash = eth::send_value(
        &world.rpc.url(primary),
        Address::repeat_byte(0x7a),
        &key,
        U256::from(1u64),
    )
    .expect("mine replacement transfer at the still-canonical nonce");
    let height = world
        .rpc
        .receipt_block_number(&hash, primary)
        .expect("replacement transfer inclusion height");
    for port in world.validators.committee_ports() {
        assert!(
            world
                .rpc
                .wait_finalized_at_least(port, height.saturating_add(2), 60),
            "validator on port {port} did not finalize beyond pending eviction"
        );
    }
}
