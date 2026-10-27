//! Dedicated hardware acceptance for the production encrypted Tribute path.

use alloy_primitives::{Address, Bytes, U256};
use cucumber::{then, when};
use outbe_primitives::entity_codec::decode_stored_tribute_v2;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::internal::{addresses, eth, tribute_keys};
use crate::world::World;

fn creator() -> Address {
    Address::repeat_byte(0x79)
}

#[when("the operator offers a Tribute owned by a different creator")]
fn offer_for_creator(world: &mut World) {
    let key = world.validators.get(0).evm_key().expect("operator key");
    assert_ne!(eth::address_of(&key).unwrap(), creator());
    super::l2_registration::ensure_tribute_offer_operator(world, &key);
    let day = world.state.wwd.as_deref().expect("offering day");
    world.state.tribute_tx_hash = Some(
        world
            .rpc
            .tribute_offer_for_creator(&key, day, creator())
            .expect("offer for creator"),
    );
}

#[when("validator 3 enclave is stopped while an encrypted Tribute is submitted")]
fn offer_during_enclave_outage(world: &mut World) {
    let key = world.validators.get(0).evm_key().expect("operator key");
    super::l2_registration::ensure_tribute_offer_operator(world, &key);
    let day = world.state.wwd.clone().expect("offering day");
    world
        .localnet
        .stop_validator_enclave_for_fault(3)
        .expect("stop validator-3 enclave sidecar");
    world.state.tribute_tx_hash = Some(
        world
            .rpc
            .tribute_offer_for_creator(&key, &day, creator())
            .expect("offer encrypted Tribute through surviving validator"),
    );
}

#[then("the surviving quorum finalizes and decrypts that encrypted Tribute")]
fn survivors_finalize_encrypted_tribute(world: &mut World) {
    let tx = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("encrypted offer transaction");
    let primary = world.validators.primary_port();
    assert!(
        world.rpc.wait_successful_receipt(tx, 240),
        "encrypted offer did not produce a successful receipt: {tx}"
    );
    let height = world
        .rpc
        .receipt_block_number(tx, primary)
        .expect("offer inclusion height");
    let ports = [0, 1, 2].map(|index| world.validators.http_port(index));
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 90)
        .expect("surviving quorum finalizes encrypted offer");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(projected) = world.projection.projected_tribute(0, tx) {
            let body = decode_stored_tribute_v2(&projected.stored_body)
                .expect("canonical encrypted body after enclave outage");
            let expected = world
                .rpc
                .private_tribute_read(0, &body)
                .expect("surviving enclave decrypts Tribute");
            for index in [1, 2] {
                assert_eq!(
                    world
                        .rpc
                        .private_tribute_read(index, &body)
                        .expect("surviving enclave decrypts Tribute"),
                    expected
                );
                let observed = world
                    .rpc
                    .checkpoint_at(world.validators.http_port(index), checkpoint.height)
                    .expect("survivor checkpoint");
                assert_eq!(observed.block_hash, checkpoint.block_hash);
                assert_eq!(observed.state_root, checkpoint.state_root);
            }
            return;
        }
        assert!(
            Instant::now() < deadline,
            "surviving validators did not project encrypted Tribute"
        );
        sleep(Duration::from_millis(250));
    }
}

#[when("validator 3 enclave is restored from its sealed state")]
fn restore_enclave_after_offer(world: &mut World) {
    if !world.localnet.validator_running(3) {
        let tx = world
            .state
            .tribute_tx_hash
            .clone()
            .expect("encrypted offer transaction");
        let primary = world.validators.primary_port();
        let receipt = eth::raw_json_result(
            &world.rpc.url(primary),
            "eth_getTransactionReceipt",
            serde_json::json!([&tx]),
        )
        .expect("encrypted offer receipt");
        let outcome = crate::world::rpc::TxOutcome {
            transaction_hash: tx.clone(),
            success: true,
            receipt,
        };
        let ports = [0, 1, 2].map(|index| world.validators.http_port(index));
        world
            .rpc
            .finalize_outcome(&outcome, &ports, 90)
            .expect("encrypted offer receipt matches surviving quorum");
        world.state.expected_tee_sidecar_outage = Some((
            3,
            tx,
            outcome
                .block_number()
                .expect("encrypted offer block height"),
            outcome.block_hash().expect("encrypted offer block hash"),
        ));
    }
    world
        .localnet
        .restore_validator_enclave_after_fault(3)
        .expect("restore validator-3 sealed enclave");
    let target = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("survivor finality after enclave recovery");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(world.validators.http_port(3), target, 90),
        "validator-3 did not catch up after enclave recovery"
    );
}

#[when("the first validator and enclave restart with their existing sealed state")]
fn restart_reader(world: &mut World) {
    world
        .localnet
        .restart_validator_and_enclave(0)
        .expect("restart existing node and sealed enclave");
    assert!(world
        .rpc
        .wait_bootstrapped(120, || world.localnet.ensure_committee_alive())
        .expect("restarted committee bootstrap"));
}

#[then("the creator and each enclave read encrypted Tribute values while public attributes retain ciphertext")]
fn verify_readers(world: &mut World) {
    let tx = world
        .state
        .tribute_tx_hash
        .as_deref()
        .expect("offer transaction");
    let port = world.validators.primary_port();
    let projected = world
        .projection
        .projected_tribute(0, tx)
        .expect("encrypted projection");
    let body = decode_stored_tribute_v2(&projected.stored_body).expect("canonical encrypted body");
    let network = world
        .rpc
        .tribute_network_public_key(port)
        .expect("installed common public key");
    assert_eq!(body.context.owner, creator());
    let amounts = outbe_tee::tribute_decrypt::decrypt_tribute_for_creator(
        &tribute_keys::secret(creator()),
        &network,
        &body,
    )
    .expect("creator local decryption");
    assert_eq!(amounts.issuance_amount_minor, U256::from(100_000_000));
    let (nominal, price) = super::tribute_expectations::usd_offer_terms_at(
        world,
        body.context.worldwide_day.value(),
        super::tribute_expectations::offer_height(world, tx),
        amounts.issuance_amount_minor,
    );
    assert_eq!(amounts.nominal_amount_minor, nominal);
    assert_eq!(body.context.tribute_price_minor, price);
    assert!(
        outbe_tee::tribute_decrypt::decrypt_tribute_for_creator(&[0x78; 32], &network, &body)
            .is_err()
    );
    for index in 0..world.validators.size() {
        let port = world.validators.committee_ports()[index];
        assert_eq!(world.rpc.tribute_network_public_key(port).unwrap(), network);
        assert_eq!(
            world
                .rpc
                .private_tribute_read(index, &body)
                .expect("authorized enclave read"),
            amounts
        );
        let mut tampered = body.clone();
        tampered.encrypted_amounts[9] ^= 1;
        assert!(world.rpc.private_tribute_read(index, &tampered).is_err());
    }
    let receipt = eth::receipt_json(&world.rpc.url(port), tx).expect("issuance receipt");
    let issued =
        eth::receipt_event::<eth::ITribute::TributeIssued>(&receipt, addresses::TRIBUTE_ADDR);
    assert_eq!(issued.owner, creator());
    assert_eq!(issued.issuanceAmountMinor.as_ref(), body.encrypted_amounts);
    assert_eq!(issued.nominalAmountMinor.as_ref(), body.encrypted_amounts);
    let uri: String = eth::read_call(
        &world.rpc.url(port),
        addresses::TRIBUTE_ADDR,
        &eth::ITribute::tokenURICall {
            tributeId: U256::from_be_slice(body.context.tribute_id.as_slice()),
        },
    )
    .expect("public tokenURI");
    let json: serde_json::Value = serde_json::from_str(
        uri.strip_prefix("data:application/json;utf8,")
            .expect("JSON URI"),
    )
    .expect("public encrypted attributes");
    let cipher = Bytes::from(body.encrypted_amounts.clone()).to_string();
    assert_eq!(
        json["attributes"][3]["value"],
        serde_json::json!({"ciphertext":cipher,"word":0})
    );
    assert_eq!(
        json["attributes"][4]["value"],
        serde_json::json!({"ciphertext":cipher,"word":1})
    );
}
