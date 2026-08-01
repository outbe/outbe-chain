use std::fs;
use std::path::PathBuf;

use alloy_primitives::{Address, U256};
use serde_json::json;

use crate::ocomp::fork::{
    require_startup_ocomp_fork_install, GENESIS_ACTIVE_OCOMP_HEIGHT,
    METADOSIS_STORAGE_LAYOUT_GENESIS_KEY, OCOMP_FORK_INSTALL_GENESIS_KEY,
};
use crate::ocomp::measurement::{
    arm_genesis, parse_outbe_chain_spec, validator_identities, ArmOptions,
};
use outbe_primitives::addresses::UPDATE_ADDRESS;

fn write_bootstrap_shaped_genesis(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("genesis.json");
    let genesis = json!({
        "config": {
            "chainId": 54_322_345_u64,
            "homesteadBlock": 0,
            "eip150Block": 0,
            "eip155Block": 0,
            "eip158Block": 0,
            "byzantiumBlock": 0,
            "constantinopleBlock": 0,
            "petersburgBlock": 0,
            "istanbulBlock": 0,
            "berlinBlock": 0,
            "londonBlock": 0,
            "mergeNetsplitBlock": 0,
            "terminalTotalDifficulty": 0,
            "terminalTotalDifficultyPassed": true,
            "shanghaiTime": 0,
            "cancunTime": 0,
            "pragueTime": 0,
        },
        "nonce": "0x0",
        "timestamp": "0x68ab0000",
        "extraData": "0x",
        "gasLimit": "0x1c9c380",
        "difficulty": "0x0",
        "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
        "coinbase": "0x0000000000000000000000000000000000000000",
        "alloc": {
            "1100000000000000000000000000000000000011": { "balance": "0x21e19e0c9bab2400000" }
        },
    });
    fs::write(&path, serde_json::to_vec_pretty(&genesis).unwrap()).unwrap();
    path
}

fn write_validators_json(dir: &std::path::Path, count: usize, key_len: usize) -> PathBuf {
    let path = dir.join("validators.json");
    let validators: Vec<serde_json::Value> = (0..count)
        .map(|index| {
            let address = Address::repeat_byte(0x20 + u8::try_from(index).unwrap());
            let key = vec![0x30 + u8::try_from(index).unwrap(); key_len];
            json!({
                "public_key": hex::encode(key),
                "address": format!("{address:#x}"),
                "p2p_address": format!("127.0.0.1:{}", 30_400 + index),
            })
        })
        .collect();
    fs::write(&path, serde_json::to_vec_pretty(&validators).unwrap()).unwrap();
    path
}

#[test]
fn armed_genesis_passes_the_startup_gate() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = write_bootstrap_shaped_genesis(dir.path());
    let validators = write_validators_json(dir.path(), 4, 48);

    let armed = arm_genesis(&genesis, &validators, &ArmOptions::default()).unwrap();
    assert_eq!(armed.chain_id, 54_322_345);
    assert_eq!(armed.install.activation_height, GENESIS_ACTIVE_OCOMP_HEIGHT);

    let spec = parse_outbe_chain_spec(&genesis).unwrap();
    let loaded = require_startup_ocomp_fork_install(&spec).unwrap();
    assert_eq!(loaded.as_ref(), armed.install.as_ref());
    assert_eq!(spec.genesis_hash(), armed.genesis_hash);
}

#[test]
fn arm_genesis_is_idempotent_and_rejects_a_different_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = write_bootstrap_shaped_genesis(dir.path());
    let validators = write_validators_json(dir.path(), 4, 48);

    let first = arm_genesis(&genesis, &validators, &ArmOptions::default()).unwrap();
    let bytes_after_first = fs::read(&genesis).unwrap();
    let second = arm_genesis(&genesis, &validators, &ArmOptions::default()).unwrap();
    assert_eq!(first.install_hash, second.install_hash);
    assert_eq!(first.genesis_hash, second.genesis_hash);
    assert_eq!(bytes_after_first, fs::read(&genesis).unwrap());

    // A manifest produced for a different committee must never be replaced
    // silently.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&genesis).unwrap()).unwrap();
    document["config"][OCOMP_FORK_INSTALL_GENESIS_KEY]["installHash"] =
        json!(alloy_primitives::B256::repeat_byte(0x99));
    fs::write(&genesis, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    let error = arm_genesis(&genesis, &validators, &ArmOptions::default())
        .expect_err("differing manifest must be refused");
    assert!(
        error
            .to_string()
            .contains("refusing to replace a different OCOMP fork install"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn arming_does_not_change_the_genesis_hash() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = write_bootstrap_shaped_genesis(dir.path());
    let validators = write_validators_json(dir.path(), 4, 48);

    // The hash the install binds is computed after the update-schedule
    // mutation; the two config manifests must then be hash-neutral.
    let armed = arm_genesis(&genesis, &validators, &ArmOptions::default()).unwrap();
    let spec = parse_outbe_chain_spec(&genesis).unwrap();
    assert_eq!(spec.genesis_hash(), armed.genesis_hash);
    assert_eq!(
        armed.install.request_profile.genesis_hash,
        armed.genesis_hash
    );

    let document: serde_json::Value = serde_json::from_slice(&fs::read(&genesis).unwrap()).unwrap();
    assert!(document["config"][OCOMP_FORK_INSTALL_GENESIS_KEY].is_object());
    assert!(document["config"][METADOSIS_STORAGE_LAYOUT_GENESIS_KEY].is_object());
}

#[test]
fn validator_identities_rejects_wrong_count_and_bad_bls_key() {
    let dir = tempfile::tempdir().unwrap();

    let three = write_validators_json(dir.path(), 3, 48);
    let error = validator_identities(&three).expect_err("three validators must be refused");
    assert!(
        error.to_string().contains("exactly four validators"),
        "unexpected error: {error:#}"
    );

    let short_key = write_validators_json(dir.path(), 4, 47);
    let error = validator_identities(&short_key).expect_err("47-byte key must be refused");
    assert!(
        error.to_string().contains("expected 48"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn update_schedule_arms_protocol_v1_at_activation_height() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = write_bootstrap_shaped_genesis(dir.path());
    let validators = write_validators_json(dir.path(), 4, 48);

    arm_genesis(&genesis, &validators, &ArmOptions::default()).unwrap();

    let document: serde_json::Value = serde_json::from_slice(&fs::read(&genesis).unwrap()).unwrap();
    let update_key = hex::encode(UPDATE_ADDRESS.as_slice());
    let storage = document["alloc"][&update_key]["storage"]
        .as_object()
        .expect("armed genesis creates the Update alloc container");
    assert!(
        !storage.is_empty(),
        "protocol-v1 schedule must write at least one non-zero Update slot"
    );
    for (slot, value) in storage {
        let value = value.as_str().unwrap();
        assert_ne!(
            U256::from_str_radix(value.trim_start_matches("0x"), 16).unwrap(),
            U256::ZERO,
            "slot {slot} stores a zero word"
        );
    }
}
