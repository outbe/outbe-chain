//! Dedicated production-chain acceptance for private NOD creation and exercise.
use super::entity_lifecycle::chain::settlement_read;
use crate::internal::{addresses, eth, nod_keys};
use crate::world::{rpc::FinalizedCheckpoint, World};
use alloy_primitives::{Address, U256};
use base64::Engine as _;
use cucumber::{then, when};

use alloy_sol_types::SolEvent as _;

fn owner(world: &World) -> Address {
    eth::address_of(&world.validators.get(0).evm_key().unwrap()).unwrap()
}

#[when("an operator offers a Tribute for a different validator as the encrypted NOD owner")]
fn offer_for_owner(world: &mut World) {
    let creator = owner(world);
    let operator = world.validators.get(1).evm_key().expect("operator key");
    assert_ne!(eth::address_of(&operator).unwrap(), creator);
    super::l2_registration::ensure_tribute_offer_operator(world, &operator);
    let day = world.state.wwd.clone().expect("offering day");
    super::tribute_projection::wait_for_offering(world, &day);
    world.state.tribute_tx_hash = Some(
        world
            .rpc
            .tribute_offer_for_creator(&operator, &day, creator)
            .expect("relayed creator offer"),
    );
}

#[then("the encrypted NOD matches its canonical body, public views and native issuance event")]
fn verify_encrypted_nod(world: &mut World) {
    let completed_at = world
        .state
        .lysis_completion_height
        .expect("native Lysis completion");
    let port = world.validators.primary_port();
    let owner = owner(world);
    let public = world
        .rpc
        .tribute_network_public_key(port)
        .expect("network encryption public key");
    let (_, data) = settlement_read(|| {
        world
            .rpc
            .nod_for_owner(port, owner)?
            .ok_or_else(|| "creator NOD is not visible yet".to_owned())
    })
    .expect("creator NOD");
    let encrypted = nod_keys::encrypted(&data);
    let amount = nod_keys::decrypt(world, &data);
    assert!(!amount.is_zero());
    assert!(
        outbe_tee::nod_decrypt::decrypt_nod_for_owner(&[0x19; 32], &public, &encrypted).is_err()
    );
    assert_eq!(data.owner, owner);
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), completed_at, 120)
        .expect("encrypted creation finalized on every validator");
    let checkpoint = world
        .rpc
        .checkpoint_at(port, completed_at)
        .expect("exact encrypted Nod issuance checkpoint");
    let mut expected_body = None;
    for (index, peer) in world.validators.committee_ports().into_iter().enumerate() {
        let body = assert_nod_state(world, (index, peer), &encrypted, checkpoint);
        assert_eq!(*expected_body.get_or_insert(body.clone()), body);
    }
    super::oracle_expectations::assert_native_entry_price(world, &encrypted.terms, completed_at);
    assert_native_issuance(world, &encrypted, completed_at);
}

fn assert_nod_state(
    world: &World,
    validator: (usize, u16),
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
    checkpoint: FinalizedCheckpoint,
) -> Vec<u8> {
    let (index, peer) = validator;
    let url = world.rpc.url(peer);
    let id = encrypted.terms.nod_id;
    let (item, _) = world
        .rpc
        .nod_state_at(peer, checkpoint, id)
        .expect("encrypted Nod EVM state")
        .expect("encrypted Nod is present");
    assert_eq!(&item.encrypted, encrypted);
    let body_bytes =
        outbe_primitives::entity_codec::encode_nod_item_v2(&outbe_nod::canonical_item(&item))
            .expect("canonical encrypted Nod");
    let view = world
        .rpc
        .nod_data_on(peer, id.as_slice())
        .expect("encrypted NOD public view");
    assert_eq!(&nod_keys::encrypted(&view), encrypted);
    assert_eq!(
        world
            .rpc
            .private_nod_read(index, encrypted)
            .expect("local enclave opens self-contained NOD"),
        nod_keys::decrypt(world, &view)
    );
    assert_encrypted_metadata(&url, encrypted);
    body_bytes
}

fn assert_encrypted_metadata(
    url: &str,
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
) {
    let uri: String = eth::read_call_result(
        url,
        addresses::NOD_ADDR,
        &eth::INod::tokenURICall {
            nodId: encrypted.terms.nod_id.to_u256(),
        },
    )
    .expect("encrypted NOD metadata");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let attributes = json["attributes"].as_array().unwrap();
    for (name, value) in [
        (
            "Encrypted Gratis Load",
            alloy_primitives::hex::encode_prefixed(&encrypted.encrypted_gratis_amount),
        ),
        (
            "Encrypted Creator Public Key",
            alloy_primitives::hex::encode_prefixed(&encrypted.encrypted_creator_public_key),
        ),
    ] {
        assert!(
            attributes
                .iter()
                .any(|attribute| attribute["trait_type"] == name && attribute["value"] == value),
            "encrypted attribute missing: {name}"
        );
    }
    assert!(
        !attributes
            .iter()
            .any(|attribute| attribute["trait_type"] == "Gratis Load"),
        "clear amount attribute remained"
    );
}

#[when("the encrypted NOD is paid and exercised through a relayer across restarts")]
fn exercise(world: &mut World) {
    // A fresh settlement asset/vault is installed through the same public fixture as B12.
    super::settlement::deploy_settlement_fixture(world);
    let day = world
        .state
        .wwd
        .as_deref()
        .expect("native Lysis day")
        .parse()
        .expect("numeric WWD");
    super::settlement::exercise_encrypted_nod(world, day);
}

#[then("the removed Gratis supply selector rejects calls on every validator")]
fn supply_removed(world: &mut World) {
    alloy_sol_types::sol! { function totalSupply() external view returns (uint256); }
    for port in world.validators.committee_ports() {
        let url = world.rpc.url(port);
        let height = eth::block_number_result(&url).expect("validator RPC remains available");
        eth::read_call_revert_data_at(
            &url,
            addresses::GRATIS_ADDR,
            owner(world),
            &totalSupplyCall {},
            crate::internal::eth::RevertAt {
                value: U256::ZERO,
                height,
            },
        )
        .expect("removed Gratis supply getter must produce an EVM revert");
    }
}

fn assert_native_issuance(
    world: &World,
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
    height: u64,
) {
    let url = world.rpc.url(world.validators.primary_port());
    let logs = eth::raw_json_result(
        &url,
        "eth_getLogs",
        serde_json::json!([{
            "address": format!("{:#x}", addresses::NOD_FACTORY_ADDR),
            "fromBlock": format!("0x{height:x}"), "toBlock": format!("0x{height:x}"),
            "topics": [format!("{:#x}", eth::INodFactory::NodIssued::SIGNATURE_HASH)]
        }]),
    )
    .expect("native Nod issuance logs");
    let mut found = 0;
    for log in logs.as_array().expect("issuance logs array") {
        let topics = log["topics"]
            .as_array()
            .expect("event topics")
            .iter()
            .map(|value| value.as_str().unwrap().parse().unwrap())
            .collect::<Vec<alloy_primitives::B256>>();
        let data: alloy_primitives::Bytes = log["data"].as_str().unwrap().parse().unwrap();
        let issued =
            eth::INodFactory::NodIssued::decode_raw_log(topics, &data).expect("native NodIssued");
        if issued.nodId == encrypted.terms.nod_id.to_u256() {
            assert_eq!(issued.owner, encrypted.terms.owner);
            assert_eq!(
                issued.encryptedGratisAmount.as_ref(),
                encrypted.encrypted_gratis_amount
            );
            found += 1;
        }
    }
    assert_eq!(found, 1, "one native issuance for this encrypted Nod");
}

#[then("the FullNode verifies the same encrypted Nod against finalized EVM state")]
fn verify_full_node_nod(world: &mut World) {
    let primary = world.validators.primary_port();
    let follower = world.validators.http_port(14);
    let height = world
        .state
        .lysis_completion_height
        .expect("native Lysis completion");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&[primary, follower], height, 120)
        .expect("FullNode native completion checkpoint");
    let (id, _) = world
        .rpc
        .nod_for_owner(primary, owner(world))
        .expect("Nod owner read")
        .expect("created Nod");
    let id = outbe_primitives::wwd_entity_id::WwdEntityId::from(U256::from_be_slice(&id));
    let expected = world
        .rpc
        .nod_state_at(primary, checkpoint, id)
        .expect("primary Nod MPT proof")
        .expect("primary Nod");
    let actual = world
        .rpc
        .nod_state_at(follower, checkpoint, id)
        .expect("FullNode Nod MPT proof")
        .expect("FullNode Nod");
    assert_eq!(
        outbe_nod::canonical_item(&actual.0),
        outbe_nod::canonical_item(&expected.0)
    );
    assert_eq!(actual.1.group_key, expected.1.group_key);
}
