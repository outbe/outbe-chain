//! Protocol constants, Oracle orientation, Cycle active day, and founder Radicle binding tests.

use std::collections::BTreeSet;
use std::fs;

use alloy_primitives::U256;
use outbe_genesis_cli::inputs::{StakeSeedTerms, ValidatorSeedTerms};
use outbe_genesis_cli::network::{profiles_dir, repo_root};
use outbe_genesis_cli::seed_alloc::seed_protocol_constants;
use outbe_genesis_cli::seed_codec::{
    address_as_u256, address_bytes, asset_address, data_slot, hex32, mapping_key,
    parse_header_timestamp, u32_bytes, StorageBuilder,
};
use outbe_genesis_cli::seed_economy::{clear_seeded_metadosis_days, seed_cycle};
use outbe_genesis_cli::seed_oracle::{seed_oracle, DEFAULT_POLICY_RATES, REFERENCE_CURRENCIES};
use outbe_genesis_cli::seed_validators::{seed_staking, seed_validator_set};
use serde_json::{json, Value};

#[test]
fn oracle_seed_preserves_configured_generic_pair_orientation() {
    let mut storage = StorageBuilder::new();
    let asset_contract_address = "0x1111111111111111111111111111111111111111";
    let cfg = json!({
        "pairs": [{"base": asset_contract_address, "quote": "840"}]
    });
    seed_oracle(&mut storage, &cfg).expect("seed_oracle");

    let slot_hex = mapping_key(&u32_bytes(1), 43);
    let slot = U256::from_str_radix(slot_hex.strip_prefix("0x").expect("0x"), 16).expect("u256");
    let expected_base = U256::from_be_slice(&asset_address(asset_contract_address).expect("addr"));
    let expected_quote = U256::from_be_slice(&asset_address("840").expect("addr"));
    assert_eq!(storage.entries[&hex32(slot)], hex32(expected_base));
    assert_eq!(
        storage.entries[&hex32(slot + U256::from(1))],
        hex32(expected_quote)
    );
}

#[test]
fn oracle_seed_rejects_iso_to_coen_and_inverse_duplicates() {
    let iso_to_coen = json!({"pairs": [{"base": "840", "quote": "COEN"}]});
    let err = seed_oracle(&mut StorageBuilder::new(), &iso_to_coen).expect_err("iso_to_coen");
    assert!(err.to_string().contains("COEN base"));

    let inverted = json!({
        "pairs": [
            {"base": "840", "quote": "0x1111111111111111111111111111111111111111"},
            {"base": "0x1111111111111111111111111111111111111111", "quote": "840"}
        ]
    });
    let err = seed_oracle(&mut StorageBuilder::new(), &inverted).expect_err("inverted");
    assert!(err.to_string().contains("inverted"));
}

#[test]
fn oracle_seed_rejects_pairs_whose_assets_resolve_to_the_same_address() {
    let asset_contract_address = "0x1111111111111111111111111111111111111111";
    for (base, quote) in [
        ("COEN", "native"),
        ("840", "840"),
        (asset_contract_address, asset_contract_address),
    ] {
        let mut storage = StorageBuilder::new();
        let cfg = json!({"pairs": [{"base": base, "quote": quote}]});
        let err = seed_oracle(&mut storage, &cfg).expect_err("expected same asset error");
        assert!(err.to_string().contains("same asset"));
        assert!(storage.entries.is_empty());
    }
}

#[test]
fn checked_in_genesis_fixtures_use_scale18_native_balances() {
    let root = repo_root();
    let node_fixture: Value = serde_json::from_str(
        &fs::read_to_string(root.join("crates/blockchain/node/tests/assets/genesis.json"))
            .expect("read node genesis"),
    )
    .expect("parse node genesis");
    let alloc = node_fixture["alloc"].as_object().expect("alloc");
    let balances: Vec<&str> = alloc
        .values()
        .map(|entry| entry["balance"].as_str().expect("balance str"))
        .collect();
    let non_zero_count = balances
        .iter()
        .filter(|&&b| b == "0xd3c21bcecceda1000000")
        .count();
    assert_eq!(non_zero_count, 21);
    let unique: BTreeSet<&str> = balances.into_iter().collect();
    assert_eq!(unique, BTreeSet::from(["0x0", "0xd3c21bcecceda1000000"]));

    let release: Value = serde_json::from_str(
        &fs::read_to_string(root.join("release/testnet-genesis.json"))
            .expect("read release genesis"),
    )
    .expect("parse release genesis");
    assert_eq!(release["alloc"], json!({}));
}

#[test]
fn checked_in_seed_profiles_split_native_and_protocol_units() {
    let root = repo_root();
    let lowstake: Value = serde_json::from_str(
        &fs::read_to_string(root.join("testing/fixtures/seed-testnet-lowstake.json"))
            .expect("read lowstake"),
    )
    .expect("parse lowstake");
    let baseline = outbe_genesis_cli::yaml::load_yaml(&profiles_dir().join("testnet.yaml"))
        .expect("load testnet.yaml");

    let profiles = [
        (
            &lowstake,
            "10000000000000000000000000",
            "10000000000000000000000000",
        ),
        (
            &baseline,
            "10000000000000000000000000",
            "10000000000000000000000000",
        ),
    ];

    for (seed, min_stake, validator_stake) in profiles {
        let balance_values: BTreeSet<&str> = seed["balance"]
            .as_object()
            .expect("balance")
            .values()
            .map(|v| v.as_str().expect("str"))
            .collect();
        assert_eq!(balance_values, BTreeSet::from(["1000000000000000000000"]));
        assert!(seed.get("gems").is_none());
        assert!(seed.get("metadosis").is_none());
        assert_eq!(seed["staking"]["min_stake"], min_stake);
        assert_eq!(seed["staking"]["genesis_validator_stake"], validator_stake);
        assert_eq!(seed["oracle"]["pairs"][0]["initial_rate"], "1000000");
        assert_eq!(seed["oracle"]["scurve_seeds"][0]["peak_price"], "1000000");
    }
}

#[test]
fn default_reference_and_policy_registries_use_the_frozen_slots() {
    let mut storage = StorageBuilder::new();
    seed_oracle(&mut storage, &json!({})).expect("seed_oracle default");

    assert!(!storage.entries.contains_key(&hex32(55u64)));
    let iso_codes = [156_u64, 344, 392, 826, 840, 978];

    assert!(!storage
        .entries
        .contains_key(&mapping_key(&u32_bytes(840), 60)));
    assert_eq!(storage.entries[&hex32(74u64)], hex32(6u64));
    for (index, &iso_code) in iso_codes.iter().enumerate() {
        let key = hex32(data_slot(74u64) + U256::from(index));
        assert_eq!(storage.entries[&key], hex32(iso_code));
    }
    let slot = mapping_key(&u32_bytes(840), 75);
    assert_eq!(storage.entries[&slot], hex32(38_800u64));
}

#[test]
fn every_reference_currency_needs_a_policy_rate() {
    let policy_rates: Vec<Value> = DEFAULT_POLICY_RATES
        .iter()
        .filter(|&&(iso_code, _)| iso_code != 978)
        .map(|&(iso_code, rate)| json!({"iso_code": iso_code, "annual_rate_1e6": rate}))
        .collect();
    let cfg = json!({ "policy_rates": policy_rates });
    let err = seed_oracle(&mut StorageBuilder::new(), &cfg).expect_err("missing policy rate");
    assert!(err.to_string().contains("no policy rate: [978]"));
}

#[test]
fn reference_currencies_match_the_oracle_constant() {
    let constants = fs::read_to_string(repo_root().join("crates/system/oracle/src/constants.rs"))
        .expect("read oracle constants");
    let line = constants
        .lines()
        .find(|line| line.starts_with("pub const REFERENCE_CURRENCIES:"))
        .expect("REFERENCE_CURRENCIES");
    let list = line.rsplit('[').next().unwrap().split(']').next().unwrap();
    let rust: Vec<u32> = list
        .split(',')
        .map(|code| code.trim().parse().unwrap())
        .collect();
    assert_eq!(REFERENCE_CURRENCIES.to_vec(), rust);
}

#[test]
fn optional_seed_profile_is_copied_to_genesis_config_and_rejects_conflicts() {
    let mut genesis = json!({"config": {"chainId": 1}, "alloc": {}});
    let profile = json!({
        "schemaVersion": 1,
        "metadosis": {"formingPeriodSeconds": 60},
        "governance": {"votingWindowBlocks": 120}
    });
    seed_protocol_constants(&mut genesis, &json!({"protocol_constants": profile}))
        .expect("seed_protocol_constants");
    assert_eq!(genesis["config"]["outbeProtocol"], profile);

    let existing = json!({"metadosis": {"formingPeriodSeconds": 60}});
    let mut genesis2 = json!({"config": {"outbeProtocol": existing}, "alloc": {}});
    seed_protocol_constants(&mut genesis2, &json!({})).expect("preserve existing");
    assert_eq!(genesis2["config"]["outbeProtocol"], existing);

    let err = seed_protocol_constants(
        &mut genesis2,
        &json!({"protocol_constants": {"metadosis": {"formingPeriodSeconds": 61}}}),
    )
    .expect_err("conflict");
    assert!(err.to_string().contains("conflicts"));

    let mut genesis3 = json!({"config": {}, "alloc": {}});
    let err = seed_protocol_constants(&mut genesis3, &json!({"protocol_constants": []}))
        .expect_err("non-object");
    assert!(err.to_string().contains("JSON object"));
}

#[test]
fn fresh_metadosis_and_cycle_active_day_work_as_expected() {
    let mut seed = json!({
        "metadosis": {"worldwide_days": [{"wwd": 20260807}], "other": 7},
        "oracle": {"scurve_seeds": [{"peak_day": 20260807}]}
    });
    clear_seeded_metadosis_days(&mut seed).expect("clear_seeded_metadosis_days");
    assert_eq!(seed["metadosis"]["worldwide_days"], json!([]));
    assert_eq!(seed["metadosis"]["other"], json!(7));
    assert_eq!(
        seed["oracle"]["scurve_seeds"],
        json!([{"peak_day": 20260807}])
    );

    let genesis = json!({
        "timestamp": format!("0x{:x}", 1_704_103_200_u64),
        "config": {"genesisTime": "2099-12-31T23:59:59Z"}
    });
    let mut storage = StorageBuilder::new();
    let ts = parse_header_timestamp(&genesis).expect("header ts");
    seed_cycle(&mut storage, ts);
    assert_eq!(storage.entries[&hex32(2u64)], hex32(20240101u64));
}

#[test]
fn founder_radicle_bindings_are_complete_unique_and_bidirectional() {
    let validators = vec![
        json!({
            "address": "0x1111111111111111111111111111111111111111",
            "public_key": "11".repeat(48),
            "radicle_node_id": "21".repeat(32),
        }),
        json!({
            "address": "0x2222222222222222222222222222222222222222",
            "public_key": "12".repeat(48),
            "radicle_node_id": "22".repeat(32),
        }),
    ];
    let terms = ValidatorSeedTerms {
        epoch_length_blocks: 300,
        epoch_start_timestamp: 1,
        min_stake: U256::from(1),
        validator_stake: U256::from(1),
    };
    let mut storage = StorageBuilder::new();
    seed_validator_set(&mut storage, &validators, &json!({}), terms).expect("seed_validator_set");

    for validator in &validators {
        let addr_str = validator["address"].as_str().expect("addr");
        let address = address_bytes(addr_str).expect("addr bytes");
        let node_id =
            hex::decode(validator["radicle_node_id"].as_str().expect("node_id")).expect("hex");
        assert_eq!(
            storage.entries[&mapping_key(&address, 50)],
            format!("0x{}", hex::encode(&node_id))
        );
        assert_eq!(
            storage.entries[&mapping_key(&node_id, 51)],
            hex32(address_as_u256(addr_str).expect("u256"))
        );
        assert!(!storage.entries.contains_key(&mapping_key(&address, 7)));
    }

    let mut staking = StorageBuilder::new();
    seed_staking(
        &mut staking,
        &validators,
        &json!({}),
        StakeSeedTerms {
            min_stake: U256::from(1),
            validator_stake: U256::from(1),
        },
    )
    .expect("seed_staking");
    for validator in &validators {
        let addr_str = validator["address"].as_str().expect("addr");
        let address = address_bytes(addr_str).expect("addr bytes");
        assert_eq!(staking.entries[&mapping_key(&address, 3)], hex32(1u64));
    }

    let mut missing = vec![validators[0].clone()];
    missing[0]
        .as_object_mut()
        .expect("obj")
        .remove("radicle_node_id");
    let err = seed_validator_set(&mut StorageBuilder::new(), &missing, &json!({}), terms)
        .expect_err("missing radicle_node_id");
    assert!(err.to_string().contains("radicle_node_id"));

    let mut duplicate = validators.clone();
    duplicate[1]["radicle_node_id"] = duplicate[0]["radicle_node_id"].clone();
    let err = seed_validator_set(&mut StorageBuilder::new(), &duplicate, &json!({}), terms)
        .expect_err("duplicate radicle_node_id");
    assert!(err.to_string().contains("duplicate Radicle NodeId"));

    let over_bound = json!({"max_validators": 257});
    let err = seed_validator_set(&mut StorageBuilder::new(), &validators, &over_bound, terms)
        .expect_err("exceeds consensus bound");
    assert!(err.to_string().contains("exceeds the consensus bound 256"));

    let mut zero = vec![validators[0].clone()];
    zero[0]["radicle_node_id"] = json!("00".repeat(32));
    let err = seed_validator_set(&mut StorageBuilder::new(), &zero, &json!({}), terms)
        .expect_err("zero node_id");
    assert!(err.to_string().contains("must not be zero"));
}
