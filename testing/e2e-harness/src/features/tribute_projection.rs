//! Focused encrypted-offer -> execution -> durable projection tracer bullet.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use cucumber::{given, then, when};
use outbe_primitives::{entity_codec::decode_stored_tribute_v2, wwd_entity_id::WwdEntityId};

use crate::features::common::boot_bounded_tribute_localnet;
use crate::features::l2_registration;
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
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    l2_registration::ensure_tribute_offer_operator(world, &key);
    wait_for_offering(world, &wwd);
    let tx_hash = world
        .rpc
        .tribute_offer(&key, &wwd)
        .expect("product CLI offerTribute returned a transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

#[when("an operator submits one encrypted tribute offer for an unregistered L2 chain")]
fn submit_one_offer_for_unregistered_chain(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    wait_for_offering(world, &wwd);
    // Supply a real proof for the canonical test L2, but do not register that
    // network. CLI inputs remain valid; chain admission must still fail closed.
    let caller = super::l2_registration::operator_address(world, &key);
    let zk = super::l2_zk_gate::offer_proof(
        world,
        caller,
        B256::with_last_byte(0x11),
        B256::with_last_byte(0x22),
        wwd.parse().expect("numeric worldwide day"),
    );
    let tx_hash = world
        .rpc
        .tribute_offer_with_zk(
            &key,
            &wwd,
            crate::world::rpc::TributeZkOffer {
                tribute_draft_id_hex: &zk.tribute_draft_id_hex,
                su_hash_hex: &zk.su_hash_hex,
                merkle_root_hex: &zk.merkle_root_hex(),
                proof_hex: &zk.proof_hex(),
                l2_chain_id: zk.l2_chain_id,
                circuit_version: zk.circuit_version,
                signature_hex: &zk.signature_hex(),
            },
        )
        .expect("product CLI offerTribute returned a transaction hash");
    world.state.l2_rejected_offer_tx_hash = Some(tx_hash);
}

#[then("the offer is rejected for an unregistered L2 chain and tribute supply stays zero")]
fn unregistered_offer_rejected(world: &mut World) {
    let tx_hash = world
        .state
        .l2_rejected_offer_tx_hash
        .as_deref()
        .expect("unregistered offer tx");
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    super::tribute_negatives::assert_rejection(
        world,
        tx_hash,
        &key,
        super::tribute_negatives::Rejection::UnregisteredNetwork,
    );
    super::tribute_negatives::assert_supply(world, 0);
}

#[when("an operator submits one encrypted cross-currency tribute offer")]
fn submit_one_cross_currency_offer(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    l2_registration::ensure_tribute_offer_operator(world, &key);
    wait_for_offering(world, &wwd);
    let tx_hash = world
        .rpc
        .tribute_cross_currency_offer(
            &key,
            crate::world::rpc::TributeOfferParams {
                wwd: &wwd,
                amounts: ("0", "410000"),
                currency: 949,
            },
            978,
        )
        .expect("native encrypted TRY/EUR offerTribute returned a transaction hash");
    world.state.tribute_tx_hash = Some(tx_hash);
}

pub(super) fn wait_for_offering(world: &World, wwd: &str) {
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
    l2_registration::ensure_tribute_offer_operator(world, &key);
    wait_for_offering(world, &wwd);
    let tx_hash = world
        .rpc
        // The first offer is TRY to EUR. Change the amount and both currencies
        // here: the second transaction must still collide because the
        // canonical Tribute identity is `(owner, worldwide_day)`.
        .tribute_offer_with_params(
            &key,
            crate::world::rpc::TributeOfferParams {
                wwd: &wwd,
                amounts: ("777", "0"),
                currency: 840,
            },
        )
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
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    for _ in 0..30 {
        if world.rpc.day_tributes(primary, &wwd).as_deref() == Some("1") {
            world
                .rpc
                .trace_tribute_state(tx_hash, "state-visible", primary);
            assert_enclave_offer_telemetry(world);
            return;
        }
        sleep(Duration::from_millis(500));
    }
    panic!("successful tribute did not raise the day's Tribute count to 1");
}

#[then("every validator stores the same Tribute and indexes in EVM state")]
fn projection_parity(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    world
        .projection
        .wait_for_tribute_projection(tx_hash, 60)
        .expect("all validator projection databases contain the same tribute");
    world.rpc.trace_tribute_state(
        tx_hash,
        "projection-visible",
        world.validators.primary_port(),
    );
    world.state.tribute_projection_before_duplicate = Some(
        world
            .projection
            .tribute_projection_snapshot(0, tx_hash)
            .expect("capture exact Tribute projection before a duplicate offer"),
    );
}

#[then("the projected Tribute has the TRY to EUR golden nominal and effective reference price")]
fn projected_cross_currency_golden(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    let projected = world
        .projection
        .projected_tribute(0, tx_hash)
        .expect("validator-0 projected Tribute body");
    let body = crate::internal::tribute_keys::calculation_view(world, &projected.stored_body)
        .expect("creator-local read of canonical projected Tribute");
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
    let key = world
        .validators
        .get(0)
        .evm_key()
        .expect("duplicate owner key");
    super::tribute_negatives::assert_rejection(
        world,
        duplicate,
        &key,
        super::tribute_negatives::Rejection::Duplicate,
    );
    super::tribute_negatives::assert_supply(world, 1);
    let original = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("original tribute tx");
    world
        .projection
        .wait_for_tribute_projection(original, 1)
        .expect("duplicate offer changed or duplicated a validator projection");
    let after = world
        .projection
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
    let expected_ids = vec![after.tribute.raw_id.to_u256()];
    for port in world.validators.committee_ports() {
        let mut owner_ids = None;
        for _ in 0..60 {
            owner_ids = world.rpc.tributes_by_owner(port, owner);
            if owner_ids.is_some() {
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
    }
}

#[then("every validator verifies the same Tribute against finalized EVM state")]
fn state_tribute_parity(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    let observed = world
        .projection
        .projected_tribute(0, tx_hash)
        .expect("stored Tribute body");
    let ports = world.validators.committee_ports();
    let height = world
        .rpc
        .receipt_block_number(tx_hash, ports[0])
        .expect("Tribute receipt height");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 60)
        .expect("shared finalized checkpoint");
    let expected =
        decode_stored_tribute_v2(&observed.stored_body).expect("stored encrypted Tribute");
    for port in ports {
        assert_eq!(
            world
                .rpc
                .authenticated_tribute_at(port, checkpoint, observed.raw_id)
                .expect("independently verified Tribute MPT proof"),
            expected
        );
    }
}

#[then("every validator reads an unknown Tribute absent from the existing day state")]
fn entity_absent_in_existing_day(world: &mut World) {
    let tx_hash = world.state.tribute_tx_hash.as_deref().expect("tribute tx");
    let observed = world
        .projection
        .projected_tribute(0, tx_hash)
        .expect("stored Tribute");
    let mut unknown: [u8; 32] = observed.raw_id.into();
    unknown[WwdEntityId::len_bytes() - 1] ^= 1;
    verify_absence_on_committee(world, WwdEntityId::from(unknown));
}

#[then("every validator reads an unknown Tribute day absent from EVM state")]
fn day_absent(world: &mut World) {
    let mut unknown = [0_u8; WwdEntityId::len_bytes()];
    unknown[..4].copy_from_slice(&20_000_101_u32.to_be_bytes());
    unknown[WwdEntityId::len_bytes() - 1] = 1;
    verify_absence_on_committee(world, WwdEntityId::from(unknown));
}

#[then("no validator stores a Tribute")]
fn no_tribute_state(world: &mut World) {
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    world
        .projection
        .assert_no_tribute_projection(&wwd)
        .expect("no Tribute state");
}

fn verify_absence_on_committee(world: &World, id: WwdEntityId) {
    let ports = world.validators.committee_ports();
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, 1, 60)
        .expect("shared finalized checkpoint");
    for port in ports {
        assert!(world
            .rpc
            .tribute_state_at(port, checkpoint, id)
            .expect("read Tribute state")
            .is_none());
    }
}

const INDEPENDENT_TRIBUTE_USER_KEY: &str =
    "0x6666666666666666666666666666666666666666666666666666666666666666";

#[when("a user without an L2 registration rejects a wrong network signature then offers through the registered network")]
fn independent_user_offers_through_network(world: &mut World) {
    use crate::internal::{eth, l2_fixture};
    use crate::world::rpc::TributeZkOffer;
    let funder = world.validators.get(0);
    let operator_key = funder.evm_key().expect("L2 network administrator key");
    l2_registration::ensure_tribute_offer_operator(world, &operator_key);
    let operator = l2_registration::operator_address(world, &operator_key);
    let caller = l2_registration::operator_address(world, INDEPENDENT_TRIBUTE_USER_KEY);
    assert_ne!(caller, operator);
    assert_eq!(world.rpc.l2_chain_by_l1_address(caller), Some(0));
    let network = world
        .rpc
        .l2_chain_by_l1_address(operator)
        .expect("administrator network");
    assert_ne!(network, 0);
    let funding = world
        .rpc
        .fund_key(&funder, INDEPENDENT_TRIBUTE_USER_KEY, 100)
        .expect("fund independent user for real encrypted offers");
    assert!(world.rpc.wait_successful_receipt(&funding, 120));
    let wwd = world.state.wwd.clone().expect("offering day");
    let day: u32 = wwd.parse().expect("numeric day");
    let (draft, su_hash) = l2_fixture::offer_identifiers("independent-user", caller, day);
    let zk = world.rpc.prove_offer_for_network(
        network,
        crate::world::rpc::TributeProofInput {
            caller,
            worldwide_day: day,
            tribute_currency: 840,
            amounts: ("100", "0"),
            draft_id: draft,
            su_hash,
        },
    );
    let wrong_signature = l2_fixture::sign_merkle_root(
        network.checked_add(1).expect("other network key"),
        &zk.merkle_root,
    );
    assert!(
        !l2_fixture::verify_merkle_root(
            &l2_fixture::root_signing_public_key(network),
            &zk.merkle_root,
            &wrong_signature
        ),
        "negative fixture must reach signature rejection"
    );
    wait_for_offering(world, &wwd);
    let submit = |signature: &str| {
        world
            .rpc
            .tribute_offer_with_zk(
                INDEPENDENT_TRIBUTE_USER_KEY,
                &wwd,
                TributeZkOffer {
                    tribute_draft_id_hex: &zk.tribute_draft_id_hex,
                    su_hash_hex: &zk.su_hash_hex,
                    merkle_root_hex: &zk.merkle_root_hex(),
                    proof_hex: &zk.proof_hex(),
                    l2_chain_id: zk.l2_chain_id,
                    circuit_version: zk.circuit_version,
                    signature_hex: signature,
                },
            )
            .expect("CLI submitted independent-user Tribute")
    };
    let rejected = submit(&format!("0x{}", hex::encode(wrong_signature)));
    super::tribute_negatives::assert_rejection(
        world,
        &rejected,
        INDEPENDENT_TRIBUTE_USER_KEY,
        super::tribute_negatives::Rejection::InvalidSignature,
    );
    super::tribute_negatives::assert_supply(world, 0);
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    world
        .projection
        .assert_no_tribute_projection(&wwd)
        .expect("wrong-network signature left no projection");
    assert!(l2_fixture::verify_merkle_root(
        &l2_fixture::root_signing_public_key(network),
        &zk.merkle_root,
        &zk.signature
    ));
    let accepted = submit(&zk.signature_hex());
    let tx = eth::raw_json_result(
        &world.rpc.url(world.validators.primary_port()),
        "eth_getTransactionByHash",
        serde_json::json!([accepted]),
    )
    .expect("independent user transaction");
    assert_eq!(tx["from"], serde_json::json!(format!("{caller:#x}")));
    world.state.tribute_tx_hash = Some(accepted);
}

#[then("the Tribute belongs to the independent user on every validator while the L2 administrator owns none")]
fn independent_tribute_owner_is_projected(world: &mut World) {
    let caller = l2_registration::operator_address(world, INDEPENDENT_TRIBUTE_USER_KEY);
    let operator_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("network administrator");
    let operator = l2_registration::operator_address(world, &operator_key);
    let tx = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("independent-user offer");
    let primary = world.validators.primary_port();
    let height = world
        .rpc
        .receipt_block_number(tx, primary)
        .expect("offer receipt block");
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), height, 120)
        .expect("independent-user offer finalized everywhere");
    for index in 0..world.validators.size() {
        let projected = world
            .projection
            .projected_tribute(index, tx)
            .expect("independent-user canonical Tribute projection");
        let encrypted = decode_stored_tribute_v2(&projected.stored_body)
            .expect("canonical independent-user encrypted body");
        let body = encrypted.context;
        assert_eq!(
            body.owner, caller,
            "network administrator must not receive the user's Tribute"
        );
        assert_eq!(body.tribute_id, projected.raw_id);
        assert_eq!(
            body.worldwide_day.value(),
            world
                .state
                .wwd
                .as_ref()
                .expect("offering day")
                .parse::<u32>()
                .expect("day")
        );
    }
    let mut expected_ids = None;
    for port in world.validators.committee_ports() {
        let ids = world
            .rpc
            .tributes_by_owner(port, caller)
            .expect("user Tribute index");
        assert_eq!(ids.len(), 1);
        assert_eq!(*expected_ids.get_or_insert(ids.clone()), ids);
        assert!(world
            .rpc
            .tributes_by_owner(port, operator)
            .expect("administrator index")
            .is_empty());
    }
    assert_eq!(
        world.rpc.l2_chain_by_l1_address(caller),
        Some(0),
        "offer must not register its caller"
    );
}

fn assert_enclave_offer_telemetry(world: &World) {
    for index in 0..world.validators.size() {
        let mut telemetry_visible = false;
        for _ in 0..60 {
            if world
                .localnet
                .enclave_log_has(index, "req=process_tribute_offer_batch")
                .expect("read required owned process log")
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
}
