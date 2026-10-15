//! Ordered nonce sequences stay pending until mined in canonical order.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use cucumber::{then, when};

use crate::internal::eth;
use crate::world::World;

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
