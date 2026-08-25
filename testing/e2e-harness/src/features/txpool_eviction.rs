//! Steps for `features/txpool_eviction.feature` — pool lifetime bounds.

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
        if world.rpc.txpool_has(port, &hash) {
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
        !world.rpc.txpool_has(port, &hash),
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
            !world.rpc.txpool_has(port, &hash),
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
