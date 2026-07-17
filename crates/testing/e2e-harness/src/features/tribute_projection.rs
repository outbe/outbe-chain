//! Focused encrypted-offer -> execution -> MongoDB projection tracer bullet.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};
use outbe_compressed_entities::{
    verify_point_read_v1, AbsentEvidenceV1, EntityId36, PointReadRequestV1, PointReadResultV1,
    VerifiedPointReadV1,
};

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

#[when("the operator submits a duplicate logical tribute offer for the same day")]
fn submit_duplicate_offer(world: &mut World) {
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
        .expect("replayed offerTribute returned transaction hash");
    world.state.duplicate_tribute_tx_hash = Some(tx_hash);
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

#[then("the duplicate is rejected without changing tribute state or projections")]
fn duplicate_rejected_without_effects(world: &mut World) {
    let duplicate = world
        .state
        .duplicate_tribute_tx_hash
        .as_deref()
        .expect("duplicate tribute tx");
    assert!(
        world.rpc.wait_receipt_status(duplicate, false, 60),
        "duplicate tribute transaction did not produce a reverted receipt: {duplicate}"
    );
    let primary = world.validators.primary_port();
    assert_eq!(
        world.rpc.supply(primary).as_deref(),
        Some("1"),
        "duplicate offer changed Tribute total supply"
    );
    let original = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("original tribute tx");
    world
        .mongodb
        .wait_for_tribute_projection(original, 1)
        .expect("duplicate offer changed or duplicated a validator projection");
}

#[then("every validator serves the same independently verified compressed tribute")]
fn compressed_tribute_parity(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    let projected = world
        .mongodb
        .projected_tribute(0, tx_hash)
        .expect("validator-0 projected Tribute body");
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: projected.raw_id,
    };
    for port in world.validators.committee_ports() {
        let chain_id = world.rpc.chain_id(port).expect("validator chain ID");
        let mut observed = None;
        let mut verified = false;
        for _ in 0..60 {
            if let Ok(package) = world.rpc.compressed_entity(port, request) {
                observed = Some(format!("{:?}", package.result));
                if matches!(
                    verify_point_read_v1(chain_id, request, &package.header, &package.result),
                    Ok(VerifiedPointReadV1::Present)
                ) {
                    let PointReadResultV1::Present { body_bytes, .. } = &package.result else {
                        unreachable!("verified Present must carry a present package")
                    };
                    assert_eq!(
                        body_bytes.as_ref(),
                        projected.stored_body,
                        "RPC body must equal Mongo bytes"
                    );
                    verified = true;
                    break;
                }
            }
            sleep(Duration::from_millis(500));
        }
        assert!(
            verified,
            "validator on port {port} did not expose the projected Tribute at a finalized header; last result: {observed:?}"
        );
    }
}

#[then("every validator proves an unknown tribute absent from the existing collection")]
fn entity_absent_in_existing_collection(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    let projected = world
        .mongodb
        .projected_tribute(0, tx_hash)
        .expect("validator-0 projected Tribute body");
    let mut unknown = projected.raw_id.into_bytes();
    unknown[EntityId36::LEN - 1] ^= 1;
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: EntityId36::try_from(unknown.as_slice()).expect("36-byte synthetic identity"),
    };
    verify_absence_on_committee(world, request, false);
}

#[then("every validator proves an unknown tribute collection absent")]
fn collection_absent(world: &mut World) {
    let mut unknown = [0_u8; EntityId36::LEN];
    unknown[..4].copy_from_slice(&20_000_101_u32.to_be_bytes());
    unknown[EntityId36::LEN - 1] = 1;
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: EntityId36::try_from(unknown.as_slice()).expect("36-byte synthetic identity"),
    };
    verify_absence_on_committee(world, request, true);
}

#[then("no validator projects a tribute")]
fn no_tribute_projection(world: &mut World) {
    world
        .mongodb
        .assert_no_tribute_projection()
        .expect("no primary or secondary Tribute projections");
}

fn verify_absence_on_committee(
    world: &World,
    request: PointReadRequestV1,
    expect_collection_absent: bool,
) {
    for port in world.validators.committee_ports() {
        let chain_id = world.rpc.chain_id(port).expect("validator chain ID");
        let mut observed = None;
        let mut verified = false;
        for _ in 0..60 {
            if let Ok(package) = world.rpc.compressed_entity(port, request) {
                observed = Some(format!("{:?}", package.result));
                let expected_scope = matches!(
                    &package.result,
                    PointReadResultV1::Absent {
                        evidence: AbsentEvidenceV1::CollectionAbsent { .. },
                        ..
                    }
                ) == expect_collection_absent;
                if expected_scope
                    && matches!(
                        verify_point_read_v1(chain_id, request, &package.header, &package.result),
                        Ok(VerifiedPointReadV1::Absent)
                    )
                {
                    verified = true;
                    break;
                }
            }
            sleep(Duration::from_millis(500));
        }
        assert!(
            verified,
            "validator on port {port} did not expose the expected verifiable absence; last result: {observed:?}"
        );
    }
}
