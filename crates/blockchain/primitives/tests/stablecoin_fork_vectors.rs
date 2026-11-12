use std::str::FromStr;

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolValue;
use outbe_primitives::stablecoin_fork::{
    stablecoin_v1_budget_for_measurement, STABLECOIN_EIP712_DOMAIN_VERSION,
    STABLECOIN_V1_SUPPORTED_NETWORKS,
};
use serde::Deserialize;
use serde_json::Value;

const FORK_MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/stablecoin/v1/fork-manifest.json"
));

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Eip712Vectors {
    generator: String,
    domain_type: String,
    domain_typehash: String,
    version: String,
    version_hash: String,
    vectors: Vec<Eip712Vector>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Eip712Vector {
    name: String,
    name_hash: String,
    chain_id: u64,
    verifying_contract: String,
    domain_separator: String,
}

#[test]
fn fork_manifest_contains_no_provisional_values() {
    let manifest = std::str::from_utf8(FORK_MANIFEST).unwrap();
    for forbidden in ["TBD", "PLACEHOLDER", "u64::MAX", "choose-later"] {
        assert!(
            !manifest.contains(forbidden),
            "fork manifest contains provisional value {forbidden}"
        );
    }
    let manifest: Value = serde_json::from_slice(FORK_MANIFEST).unwrap();
    assert_eq!(
        manifest["activation"]["supportedNetworks"]
            .as_array()
            .unwrap()
            .len(),
        STABLECOIN_V1_SUPPORTED_NETWORKS.len()
    );
    assert!(!manifest["reopenRules"].as_array().unwrap().is_empty());
}

#[test]
fn benchmark_margin_rounds_up_and_detects_overflow() {
    assert_eq!(stablecoin_v1_budget_for_measurement(0), Some(0));
    assert_eq!(stablecoin_v1_budget_for_measurement(8_000), Some(10_000));
    assert_eq!(stablecoin_v1_budget_for_measurement(8_001), Some(20_000));
    assert_eq!(stablecoin_v1_budget_for_measurement(u64::MAX), None);
}

#[test]
fn independent_foundry_domain_vectors_pin_version_chain_and_token() {
    let fixture: Eip712Vectors = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/stablecoin/v1/eip712-domain-vectors.json"
    )))
    .unwrap();
    assert_eq!(fixture.generator, "Foundry cast 1.7.1");
    let typehash = keccak256(fixture.domain_type.as_bytes());
    let version_hash = keccak256(STABLECOIN_EIP712_DOMAIN_VERSION.as_bytes());
    assert_eq!(typehash, B256::from_str(&fixture.domain_typehash).unwrap());
    assert_eq!(fixture.version, STABLECOIN_EIP712_DOMAIN_VERSION);
    assert_eq!(version_hash, B256::from_str(&fixture.version_hash).unwrap());

    assert!(
        fixture
            .vectors
            .iter()
            .any(|vector| vector.chain_id == outbe_primitives::chain::MAINNET_CHAIN_ID),
        "the independent corpus must include the Mainnet EIP-712 domain"
    );

    for vector in fixture.vectors {
        let name_hash = keccak256(vector.name.as_bytes());
        assert_eq!(name_hash, B256::from_str(&vector.name_hash).unwrap());
        let address = Address::from_str(&vector.verifying_contract).unwrap();
        let encoded = (
            typehash,
            name_hash,
            version_hash,
            U256::from(vector.chain_id),
            address,
        )
            .abi_encode();
        assert_eq!(
            keccak256(encoded),
            B256::from_str(&vector.domain_separator).unwrap()
        );
    }
}
