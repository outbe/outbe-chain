use std::str::FromStr;

use alloy_primitives::{Address, B256};
use outbe_primitives::{
    addresses::{
        is_stablecoin_address, STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_MARKER_CODE, STABLECOIN_POLICY_REGISTRY_ADDRESS,
    },
    chain::{DEVNET_CHAIN_ID, DEVNET_CHAIN_NAME, TESTNET_CHAIN_ID, TESTNET_CHAIN_NAME},
    stablecoin::{predict_stablecoin, stablecoin_token_id_preimage},
};
use serde::Deserialize;

const ADDRESSES_SOURCE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/addresses.rs"));
const NETWORK_VECTORS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/stablecoin/v1/network-address-vectors.json"
));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkCorpus {
    generator: String,
    factory: String,
    policy_registry: String,
    prefix: String,
    marker: String,
    networks: Vec<NetworkVector>,
    mainnet: MainnetStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkVector {
    name: String,
    chain_id: u64,
    issuer: String,
    ticker: String,
    preimage: String,
    token_id: String,
    token: String,
}

#[derive(Deserialize)]
struct MainnetStatus {
    status: String,
}

#[test]
fn stablecoin_constants_are_exact_and_class_disjoint() {
    assert_eq!(
        STABLECOIN_FACTORY_ADDRESS,
        Address::from_str("0x000000000000000000000000000000000000ee0f").unwrap()
    );
    assert_eq!(
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
        Address::from_str("0x000000000000000000000000000000000000ee10").unwrap()
    );
    assert_eq!(STABLECOIN_ADDRESS_PREFIX, [0x53, 0xc0]);
    assert_eq!(STABLECOIN_MARKER_CODE, [0xef]);
    assert!(!is_stablecoin_address(STABLECOIN_FACTORY_ADDRESS));
    assert!(!is_stablecoin_address(STABLECOIN_POLICY_REGISTRY_ADDRESS));
}

#[test]
fn every_declared_exact_address_is_unique_and_outside_the_dynamic_class() {
    let mut addresses = address_literals(ADDRESSES_SOURCE);
    let original_len = addresses.len();
    addresses.sort_unstable();
    addresses.dedup();
    assert_eq!(addresses.len(), original_len, "duplicate address literal");
    assert!(
        addresses
            .iter()
            .copied()
            .all(|address| !is_stablecoin_address(address)),
        "fixed address collides with stablecoin class"
    );

    for suffix in 1u8..=10 {
        let builtin = Address::with_last_byte(suffix);
        assert!(!addresses.contains(&builtin));
        assert!(!is_stablecoin_address(builtin));
    }
}

#[test]
fn independent_network_vectors_pin_current_supported_geneses() {
    let corpus: NetworkCorpus = serde_json::from_str(NETWORK_VECTORS).unwrap();
    assert!(corpus.generator.starts_with("Foundry cast"));
    assert_eq!(
        Address::from_str(&corpus.factory).unwrap(),
        STABLECOIN_FACTORY_ADDRESS
    );
    assert_eq!(
        Address::from_str(&corpus.policy_registry).unwrap(),
        STABLECOIN_POLICY_REGISTRY_ADDRESS
    );
    assert_eq!(decode_prefix(&corpus.prefix), STABLECOIN_ADDRESS_PREFIX);
    assert_eq!(
        hex::decode(corpus.marker.trim_start_matches("0x")).unwrap(),
        STABLECOIN_MARKER_CODE
    );
    assert!(corpus.mainnet.status.starts_with("unsupported"));

    for vector in corpus.networks {
        match vector.name.as_str() {
            DEVNET_CHAIN_NAME => assert_eq!(vector.chain_id, DEVNET_CHAIN_ID),
            TESTNET_CHAIN_NAME => assert_eq!(vector.chain_id, TESTNET_CHAIN_ID),
            other => panic!("unexpected network vector {other}"),
        }
        let issuer = Address::from_str(&vector.issuer).unwrap();
        let expected_preimage = hex::decode(vector.preimage.trim_start_matches("0x")).unwrap();
        let expected_id = B256::from_str(&vector.token_id).unwrap();
        let expected_token = Address::from_str(&vector.token).unwrap();
        assert_eq!(
            stablecoin_token_id_preimage(
                vector.chain_id,
                STABLECOIN_FACTORY_ADDRESS,
                issuer,
                &vector.ticker,
            )
            .unwrap(),
            expected_preimage
        );
        assert_eq!(
            predict_stablecoin(
                vector.chain_id,
                STABLECOIN_FACTORY_ADDRESS,
                issuer,
                &vector.ticker,
                STABLECOIN_ADDRESS_PREFIX,
            )
            .unwrap(),
            (expected_id, expected_token)
        );
        assert!(is_stablecoin_address(expected_token));
    }
}

fn address_literals(source: &str) -> Vec<Address> {
    let marker = "address!(\"0x";
    let mut remaining = source;
    let mut addresses = Vec::new();
    while let Some(offset) = remaining.find(marker) {
        let hex_start = offset + marker.len();
        let hex_end = hex_start + 40;
        let Some(value) = remaining.get(hex_start..hex_end) else {
            break;
        };
        if let Ok(address) = Address::from_str(value) {
            addresses.push(address);
        }
        remaining = &remaining[hex_end..];
    }
    addresses
}

fn decode_prefix(value: &str) -> [u8; 2] {
    let mut prefix = [0u8; 2];
    hex::decode_to_slice(value.trim_start_matches("0x"), &mut prefix).unwrap();
    prefix
}
