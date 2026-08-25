//! Focused encrypted-offer -> execution -> MongoDB projection tracer bullet.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use cucumber::{given, then, when};
use outbe_compressed_entities::{
    decode_stored_tribute_v1, verify_point_read_v1, AbsentEvidenceV1, PointReadRequestV1,
    PointReadResultV1, VerifiedPointReadV1, WwdEntityId,
};

use crate::features::common::boot_bounded_tribute_localnet;
use crate::world::World;

#[given(
    expr = "a fresh localnet with cross-currency Tribute pricing and a {int}-block voting window"
)]
fn fresh_cross_currency_tribute_localnet(world: &mut World, window: u64) {
    fresh_bounded_tribute_localnet(world, window);
}

#[given(expr = "a fresh localnet with a bounded Tribute offering and a {int}-block voting window")]
fn fresh_bounded_tribute_offering_localnet(world: &mut World, window: u64) {
    fresh_bounded_tribute_localnet(world, window);
}

fn fresh_bounded_tribute_localnet(world: &mut World, window: u64) {
    boot_bounded_tribute_localnet(world, window, &[]);
}

#[when("an operator submits one encrypted tribute offer")]
fn submit_one_offer(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    wait_for_offering(world, &wwd);
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    let tx_hash = world
        .rpc
        .tribute_offer(&key, &wwd)
        .expect("product CLI offerTribute returned a transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

#[when("an operator submits one encrypted cross-currency tribute offer")]
fn submit_one_cross_currency_offer(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    wait_for_offering(world, &wwd);
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    let tx_hash = world
        .rpc
        .tribute_cross_currency_offer(&key, &wwd, "0", "410000", 949, 978, false)
        .expect("native encrypted TRY/EUR offerTribute returned a transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

fn wait_for_offering(world: &World, wwd: &str) {
    let worldwide_day = wwd
        .parse::<u32>()
        .expect("valid worldwide-day set at setup");
    let primary = world.validators.primary_port();
    let mut offering = false;
    for _ in 0..240 {
        let state = world
            .rpc
            .metadosis_wwd_state_on(primary, worldwide_day)
            .expect("read authoritative worldwide-day state before Tribute submission");
        let head_timestamp = world
            .rpc
            .latest_block_timestamp(primary)
            .expect("read canonical head timestamp before Tribute submission");
        assert!(
            head_timestamp < state.offering_end,
            "worldwide-day {wwd} public OFFERING fixture expired before Tribute submission: \
             canonical_head_timestamp={head_timestamp}, offering_end={}, status={}",
            state.offering_end,
            state.status,
        );
        if state.status == 2 {
            offering = true;
            break;
        }
        sleep(Duration::from_millis(500));
    }
    assert!(
        offering,
        "worldwide-day {wwd} did not reach authoritative OFFERING status"
    );
}

#[when("the operator submits a duplicate logical tribute offer with different parameters for the same day")]
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
        // The first offer uses amount=100 and exclude=false. Change both
        // fields here: the second transaction must still collide because the
        // canonical Tribute identity is `(owner, worldwide_day)`.
        .tribute_offer_with_params(&key, &wwd, "777", "0", 840, true)
        .expect("replayed offerTribute returned transaction hash");
    world.state.duplicate_tribute_tx_hash = Some(tx_hash);
}

#[then("the tribute transaction succeeds and supply becomes one")]
fn successful_receipt_and_supply(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    assert!(
        world.rpc.wait_successful_receipt(tx_hash, 240),
        "tribute transaction did not produce a successful receipt: {tx_hash}"
    );
    let primary = world.validators.primary_port();
    for _ in 0..30 {
        if world.rpc.supply(primary).as_deref() == Some("1") {
            world
                .rpc
                .trace_tribute_state(tx_hash, "state-visible", primary);
            // The offer just went through every enclave: the per-request
            // telemetry line must be on the enclave log, and each validator's
            // canary-fed enclave status must not be failing.
            for index in 0..world.validators.size() {
                let mut telemetry_visible = false;
                for _ in 0..60 {
                    if world
                        .localnet
                        .enclave_log_has(index, "req=process_tribute_offer_batch")
                    {
                        telemetry_visible = true;
                        break;
                    }
                    sleep(Duration::from_millis(250));
                }
                assert!(
                    telemetry_visible,
                    "validator-{index} enclave log lacks the offer telemetry line"
                );
                if let Some(raw) = world
                    .rpc
                    .consensus_status_field(world.validators.http_port(index), "enclave")
                {
                    let enclave: serde_json::Value =
                        serde_json::from_str(&raw).expect("enclave status json");
                    let state = enclave
                        .get("state")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    assert!(
                        state != "degraded" && state != "unavailable",
                        "validator-{index} enclave canary is {state} right after a \
                         successful enclave-backed offer"
                    );
                }
            }
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
    world
        .rpc
        .trace_tribute_state(tx_hash, "mongo-visible", world.validators.primary_port());
    world.state.tribute_projection_before_duplicate = Some(
        world
            .mongodb
            .tribute_projection_snapshot(0, tx_hash)
            .expect("capture exact Tribute projection before a duplicate offer"),
    );
}

#[then("the projected Tribute has the TRY to EUR golden nominal and effective reference price")]
fn projected_cross_currency_golden(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    let projected = world
        .mongodb
        .projected_tribute(0, tx_hash)
        .expect("validator-0 projected Tribute body");
    let body = decode_stored_tribute_v1(&projected.stored_body)
        .expect("decode canonical projected Tribute body");
    assert_eq!(body.issuance_amount_minor, U256::from(410_000u64));
    assert_eq!(body.issuance_currency, 949);
    assert_eq!(body.reference_currency, 978);
    assert_eq!(body.nominal_amount_minor, U256::from(31_250u64));
    assert_eq!(body.tribute_price_minor, U256::from(320_000u64));
}

#[then("the duplicate is rejected without changing tribute state or projections")]
fn duplicate_rejected_without_effects(world: &mut World) {
    let duplicate = world
        .state
        .duplicate_tribute_tx_hash
        .as_deref()
        .expect("duplicate tribute tx");
    assert!(
        world.rpc.wait_receipt_status(duplicate, false, 240),
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
    let after = world
        .mongodb
        .tribute_projection_snapshot(0, original)
        .expect("load exact Tribute projection after duplicate rejection");
    assert_eq!(
        Some(&after),
        world.state.tribute_projection_before_duplicate.as_ref(),
        "duplicate offer mutated the original primary or either secondary index"
    );

    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    let owner: Address = world
        .rpc
        .address_of(&key)
        .expect("derive duplicate owner")
        .parse()
        .expect("parse duplicate owner");
    let wwd: u32 = world
        .state
        .wwd
        .as_deref()
        .expect("worldwide day")
        .parse()
        .expect("numeric worldwide day");
    let expected_id = after.documents[0]
        .get_str("_id")
        .expect("projected primary _id");
    let expected_id = hex::decode(expected_id).expect("hex projected primary _id");
    let expected_ids = vec![alloy_primitives::U256::from_be_slice(&expected_id)];
    for port in world.validators.committee_ports() {
        let mut owner_ids = None;
        let mut day_ids = None;
        for _ in 0..60 {
            owner_ids = world.rpc.tributes_by_owner(port, owner);
            day_ids = world.rpc.tributes_by_day(port, wwd);
            if owner_ids.is_some() && day_ids.is_some() {
                break;
            }
            sleep(Duration::from_millis(500));
        }
        let owner_ids = owner_ids.expect("read owner Tribute index");
        assert_eq!(
            owner_ids.as_slice(),
            expected_ids,
            "duplicate changed the owner's single-Tribute index on port {port}"
        );
        let day_ids = day_ids.expect("read Worldwide-Day Tribute index");
        assert_eq!(
            day_ids.as_slice(),
            expected_ids,
            "duplicate added or replaced a Tribute in the day index on port {port}"
        );
    }
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
    let mut unknown: [u8; 32] = projected.raw_id.into();
    unknown[WwdEntityId::len_bytes() - 1] ^= 1;
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: WwdEntityId::from(unknown),
    };
    verify_absence_on_committee(world, request, false);
}

#[then("every validator proves an unknown tribute collection absent")]
fn collection_absent(world: &mut World) {
    let mut unknown = [0_u8; WwdEntityId::len_bytes()];
    unknown[..4].copy_from_slice(&20_000_101_u32.to_be_bytes());
    unknown[WwdEntityId::len_bytes() - 1] = 1;
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: WwdEntityId::from(unknown),
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
