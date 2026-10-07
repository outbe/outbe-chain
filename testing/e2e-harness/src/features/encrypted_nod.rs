//! Dedicated production-chain acceptance for private NOD creation and exercise.
use super::entity_lifecycle::chain::settlement_read;
use crate::internal::{addresses, eth, nod_keys};
use crate::world::World;
use alloy_primitives::{Address, U256};
use base64::Engine as _;
use cucumber::{then, when};
use outbe_compressed_entities::{
    decode_stored_nod_item_v2, verify_point_read_v1, PointReadRequestV1, PointReadResultV1,
    SelectedHeaderV1, VerifiedPointReadV1,
};
use outbe_ocomp_protocol::{
    abi::{
        decode_protected_materialize_certified_nods_calldata, MATERIALIZE_CERTIFIED_NODS_SELECTOR,
    },
    profile::poc_schema_limits,
};

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

#[then("the encrypted NOD matches its protected calldata, canonical body, public views and events")]
fn verify_encrypted_nod(world: &mut World) {
    let generation = world
        .state
        .ocomp_certified_generation
        .clone()
        .expect("real certified Lysis generation");
    let port = world.validators.primary_port();
    let complete = world
        .rpc
        .wait_for_completed_nod_materialization(port, &generation, 120)
        .expect("production encrypted materialization completes");
    let owner = owner(world);
    let public = world
        .rpc
        .tribute_network_public_key(port)
        .expect("network encryption public key");
    let (_, data) = settlement_read(|| {
        world
            .rpc
            .materialized_nod_for_owner(port, owner)?
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
        .wait_finalized_checkpoint(
            &world.validators.committee_ports(),
            complete.completion_block_number,
            120,
        )
        .expect("encrypted creation finalized on every validator");
    let mut expected_body = None;
    for (index, peer) in world.validators.committee_ports().into_iter().enumerate() {
        let body = assert_nod_projection(
            world,
            (index, peer),
            &encrypted,
            complete.completion_block_number,
        );
        assert_eq!(*expected_body.get_or_insert(body.clone()), body);
    }
    assert_materialization_carrier(world, &encrypted, complete.completion_block_number);
}

fn assert_nod_projection(
    world: &World,
    validator: (usize, u16),
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
    completed_at: u64,
) -> Vec<u8> {
    let (index, peer) = validator;
    let url = world.rpc.url(peer);
    let id = encrypted.terms.nod_id;
    let request = PointReadRequestV1 {
        domain_id: 2,
        raw_id: id,
    };
    let response = world
        .rpc
        .compressed_entity_ready(peer, request)
        .expect("canonical NOD proof");
    assert!(response.header.block_number >= completed_at);
    // Read the trust anchor independently from the canonical EVM header.
    let commitment =
        eth::block_commitment(&url, response.header.block_number).expect("canonical EVM header");
    assert_eq!(response.header.block_hash, commitment.0);
    let trusted = SelectedHeaderV1 {
        block_number: response.header.block_number,
        block_hash: commitment.0,
        extra_data: commitment.2.to_vec(),
    };
    assert_eq!(
        verify_point_read_v1(
            encrypted.terms.chain_id,
            request,
            &trusted,
            &response.result
        )
        .unwrap(),
        VerifiedPointReadV1::Present
    );
    let PointReadResultV1::Present { body_bytes, .. } = response.result else {
        panic!("encrypted NOD absent")
    };
    let canonical =
        decode_stored_nod_item_v2(body_bytes.as_ref()).expect("fresh-genesis encrypted NOD schema");
    assert_eq!(&canonical.encrypted, encrypted);
    let view = settlement_read(|| world.rpc.nod_data_on(peer, id.as_slice()))
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
    body_bytes.to_vec()
}

fn assert_encrypted_metadata(
    url: &str,
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
) {
    let uri: String = settlement_read(|| {
        eth::read_call_result(
            url,
            addresses::NOD_ADDR,
            &eth::INod::tokenURICall {
                nodId: encrypted.terms.nod_id.to_u256(),
            },
        )
    })
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

fn assert_materialization_carrier(
    world: &World,
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
    last: u64,
) {
    let first = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("real Lysis activation")
        .block_number;
    let url = world.rpc.url(world.validators.primary_port());
    let mut matches = 0;
    for height in first..=last {
        let block = eth::raw_json_result(
            &url,
            "eth_getBlockByNumber",
            serde_json::json!([format!("0x{height:x}"), true]),
        )
        .expect("materialization block");
        for tx in block["transactions"].as_array().expect("transactions") {
            matches += assert_encrypted_creation(&url, tx, encrypted);
        }
    }
    assert_eq!(matches, 1, "one finalized encrypted creation");
}

fn assert_encrypted_creation(
    url: &str,
    tx: &serde_json::Value,
    encrypted: &outbe_primitives::nod_encryption::EncryptedNodV2,
) -> usize {
    if tx["to"]
        .as_str()
        .and_then(|value| value.parse::<Address>().ok())
        != Some(addresses::NOD_FACTORY_ADDR)
    {
        return 0;
    }
    let input = hex::decode(tx["input"].as_str().unwrap().trim_start_matches("0x")).unwrap();
    if input.get(..4) != Some(MATERIALIZE_CERTIFIED_NODS_SELECTOR.as_slice()) {
        return 0;
    }
    let carrier =
        decode_protected_materialize_certified_nods_calldata(&input, &poc_schema_limits())
            .expect("public calldata carries encrypted NME2 only");
    let mut matches = 0;
    for record in carrier.encrypted_nods {
        let nod: outbe_primitives::nod_encryption::EncryptedNodV2 =
            serde_json::from_slice(&record.0).expect("self-contained encrypted NOD record");
        assert_eq!(serde_json::to_vec(&nod).unwrap(), record.0);
        if nod.terms.nod_id == encrypted.terms.nod_id {
            assert_eq!(&nod, encrypted);
            let receipt = eth::receipt_json(url, tx["hash"].as_str().unwrap())
                .expect("materialization receipt");
            let issued = eth::receipt_event::<eth::INodFactory::NodIssued>(
                &receipt,
                addresses::NOD_FACTORY_ADDR,
            );
            assert_eq!(issued.owner, encrypted.terms.owner);
            assert_eq!(
                issued.encryptedGratisAmount.as_ref(),
                encrypted.encrypted_gratis_amount
            );
            matches += 1;
        }
    }
    matches
}

#[when("the encrypted NOD is paid and exercised through a relayer across restarts")]
fn exercise(world: &mut World) {
    // A fresh settlement asset/vault is installed through the same public fixture as B12.
    super::settlement::deploy_settlement_fixture(world);
    let day = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .unwrap()
        .worldwide_day;
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
            U256::ZERO,
            height,
        )
        .expect("removed Gratis supply getter must produce an EVM revert");
    }
}
