//! Focused encrypted-offer -> execution -> MongoDB projection tracer bullet.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};

use crate::world::World;

#[when("an operator submits one encrypted tribute offer")]
fn submit_one_offer(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    let tx_hash = world
        .rpc
        .tribute_offer(&key, &wwd)
        .expect("outbe-cli returned offerTribute transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

#[then("the tribute transaction succeeds and supply becomes one")]
fn successful_receipt_and_supply(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    assert!(
        world.rpc.wait_successful_receipt(tx_hash, 60),
        "tribute transaction did not produce a successful receipt: {tx_hash}"
    );
    let primary = world.validators.primary_port();
    for _ in 0..30 {
        if world.rpc.supply(primary).as_deref() == Some("1") {
            return;
        }
        sleep(Duration::from_millis(500));
    }
    panic!("successful tribute did not increase totalSupply to 1");
}

#[then("every validator projects the same tribute and indexes")]
fn projection_parity(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    world
        .mongodb
        .wait_for_tribute_projection(tx_hash, 60)
        .expect("all validator projection databases contain the same tribute");
}
