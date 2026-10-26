//! YAML subset parser, config validation, key derivation, seed merge, and seed stage tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use outbe_genesis_cli::attestation::run_tee_stage;
use outbe_genesis_cli::inputs::SeedInputs;
use outbe_genesis_cli::keys::{
    discover_validators, evm_address_from_key_file, radicle_node_id_from_public_key,
};
use outbe_genesis_cli::network::{
    base_profile_path, network_identity, profiles_dir, top_level_keys, validate_config,
    DEFAULT_CONSENSUS_P2P_PORT, DEFAULT_EPOCH_LENGTH_BLOCKS, DEFAULT_GAS_LIMIT,
    DEFAULT_PREFUND_COEN_UNITS, SEED_SECTIONS,
};
use outbe_genesis_cli::seed_codec::{
    address_as_u256, address_bytes, asset_address, data_slot, hex32, mapping_key, parse_u256,
    u32_bytes, CREDIS_ADDRESS, CREDIS_PROFILE_SLOT, NOD_ADDRESS, NOD_PROFILE_SLOT, ORACLE_ADDRESS,
    REWARDS_ADDRESS, VALIDATOR_SET_ADDRESS, VAULT_ROUTER_ADDRESS,
};
use outbe_genesis_cli::seed_genesis::apply_seed;
use outbe_genesis_cli::stages::{
    build_base_genesis, build_seed, prefund_validators, run_seed_stage,
};
use outbe_genesis_cli::yaml::{load_yaml, parse_yaml_str};
use serde_json::{json, Value};

fn parse(text: &str) -> Value {
    parse_yaml_str(Path::new("test.yaml"), text).expect("valid yaml")
}

fn parse_error(text: &str) -> String {
    parse_yaml_str(Path::new("test.yaml"), text)
        .expect_err("expected yaml parse error")
        .to_string()
}

fn minimal_config(keys_dir: &str) -> Value {
    json!({
        "chain_id": 424242,
        "validators": ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"],
        "keys_dir": keys_dir,
        "tee": {"mode": "gramine-direct-dev"},
    })
}

fn merge_obj(mut base: Value, extra: Value) -> Value {
    let base_obj = base.as_object_mut().expect("base object");
    for (k, v) in extra.as_object().expect("extra object") {
        base_obj.insert(k.clone(), v.clone());
    }
    base
}

fn write_ssh_ed25519_pub(path: &Path, node_id: &[u8]) {
    fn field(value: &[u8]) -> Vec<u8> {
        let mut out = (value.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(value);
        out
    }
    let mut payload = field(b"ssh-ed25519");
    payload.extend_from_slice(&field(node_id));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create_dir_all");
    }
    fs::write(
        path,
        format!("ssh-ed25519 {} outbe\n", BASE64.encode(payload)),
    )
    .expect("write pub");
}

fn fake_validators() -> Vec<Value> {
    (0..4)
        .map(|index| {
            json!({
                "address": format!("0x{}", (index + 1).to_string().repeat(40)),
                "public_key": format!("{:02x}", index + 1).repeat(48),
                "radicle_node_id": format!("0x{}", format!("{:02x}", index + 9).repeat(32)),
                "p2p_address": format!("10.0.0.{}:30400", index + 1),
            })
        })
        .collect()
}

fn seed_once(work_dir: &Path, config: &Value) -> Value {
    let validators = fake_validators();
    let mut base_genesis = build_base_genesis(config).expect("build_base_genesis");
    prefund_validators(&mut base_genesis, config, &validators).expect("prefund_validators");
    let seed = build_seed(config).expect("build_seed");
    let path = run_seed_stage(
        work_dir,
        &SeedInputs {
            base_genesis,
            seed,
            validators,
        },
        config,
    )
    .expect("run_seed_stage");
    serde_json::from_str(&fs::read_to_string(path).expect("read seeded")).expect("parse seeded")
}

#[test]
fn yaml_parser_scalars_lists_and_nesting() {
    let config = parse(concat!(
        "a: 1\n",
        "b: true\n",
        "c: plain string\n",
        "d: \"quoted: string\"\n",
        "nested:\n",
        "  x: 2\n",
        "  deeper:\n",
        "    y: false\n",
        "hosts:\n",
        "  - 10.0.0.1\n",
        "  - 10.0.0.2\n",
        "items:\n",
        "  - base: COEN\n",
        "    quote: \"840\"\n",
        "  - base: XYZ\n",
        "    quote: \"978\"\n",
    ));
    assert_eq!(config["a"], json!(1));
    assert_eq!(config["b"], json!(true));
    assert_eq!(config["c"], json!("plain string"));
    assert_eq!(config["d"], json!("quoted: string"));
    assert_eq!(config["nested"]["deeper"]["y"], json!(false));
    assert_eq!(config["hosts"], json!(["10.0.0.1", "10.0.0.2"]));
    assert_eq!(
        config["items"],
        json!([
            {"base": "COEN", "quote": "840"},
            {"base": "XYZ", "quote": "978"}
        ])
    );

    assert_eq!(
        parse("# leading\n\na: 1  # trailing\n\n# done\n"),
        json!({"a": 1})
    );
    assert_eq!(
        parse("balance:\n  \"0xAb\": \"10\"\n")["balance"],
        json!({"0xAb": "10"})
    );
    assert_eq!(parse("tributes: []\n"), json!({"tributes": []}));

    assert!(!parse_error("a:\n\tb: 1\n").is_empty());
    assert!(parse_error("a: 1\na: 2\n").contains("duplicate"));
    assert!(parse_error("a: 3.14\n").contains("floating-point"));
    assert!(parse_error("a: null\n").contains("null"));
    assert!(parse_error("a:\n").contains("null or empty"));
}

#[test]
fn config_validation_matrix_and_rejections() {
    validate_config(&minimal_config("./keys")).expect("minimal config");

    let err = validate_config(&merge_obj(minimal_config("./keys"), json!({"bogus": 1})))
        .expect_err("bogus key");
    assert!(err.to_string().contains("bogus"));

    let err = validate_config(&merge_obj(
        minimal_config("./keys"),
        json!({"vault_router": {"owner": format!("0x{}", "11".repeat(20))}}),
    ))
    .expect_err("vault_router key");
    assert!(err.to_string().contains("vault_router"));

    let err = validate_config(&merge_obj(
        minimal_config("./keys"),
        json!({"validators": ["10.0.0.1", "10.0.0.2", "10.0.0.3"]}),
    ))
    .expect_err("wrong validator count");
    assert!(err.to_string().contains("exactly 4"));

    let mut no_keys_dir = minimal_config("./keys");
    no_keys_dir.as_object_mut().expect("obj").remove("keys_dir");
    let err = validate_config(&no_keys_dir).expect_err("missing keys_dir");
    assert!(err.to_string().contains("keys_dir"));

    let mut non_str_val = minimal_config("./keys");
    non_str_val["validators"][1] = json!({"host": "10.0.0.2"});
    let err = validate_config(&non_str_val).expect_err("non-string validator");
    assert!(err.to_string().contains("host or IP string"));

    let mut bad_tee = minimal_config("./keys");
    bad_tee["tee"]["mode"] = json!("trust-me");
    let err = validate_config(&bad_tee).expect_err("bad tee mode");
    assert!(err.to_string().contains("tee.mode"));

    for (network, chain_id) in [("devnet", 424242_u64), ("testnet", 54322345_u64)] {
        for tee_mode in ["dcap-required", "gramine-direct-dev"] {
            let mut cfg = merge_obj(
                minimal_config("./keys"),
                json!({
                    "network": network,
                    "chain_id": chain_id,
                    "tee": {"mode": tee_mode},
                }),
            );
            if tee_mode == "dcap-required" {
                cfg["enclave_image"] =
                    json!(format!("outbe-tee-enclave@sha256:{}", "ab".repeat(32)));
            }
            validate_config(&cfg).expect("canonical attestation matrix");
        }
    }

    let mut mainnet = merge_obj(
        minimal_config("./keys"),
        json!({
            "network": "mainnet",
            "chain_id": 676,
            "tee": {"mode": "dcap-required"},
            "enclave_image": format!("outbe-tee-enclave@sha256:{}", "ab".repeat(32)),
            "price_feed_rest": "https://prices.outbe.net",
        }),
    );
    validate_config(&mainnet).expect("mainnet valid");
    assert_eq!(
        network_identity(&mainnet).expect("identity"),
        ("mainnet".to_owned(), 676, "outbe-mainnet-1".to_owned())
    );

    mainnet["tee"] = json!({"mode": "gramine-direct-dev"});
    let err = validate_config(&mainnet).expect_err("mainnet rejects direct dev");
    assert!(err.to_string().contains("Mainnet requires"));

    let unpinned_dcap = merge_obj(
        minimal_config("./keys"),
        json!({"chain_id": 424242, "tee": {"mode": "dcap-required"}}),
    );
    let err = validate_config(&unpinned_dcap).expect_err("unpinned dcap");
    assert!(err.to_string().contains("immutable digest"));

    let port_collision = merge_obj(minimal_config("./keys"), json!({"rpc_port": 9101}));
    let err = validate_config(&port_collision).expect_err("port collision");
    assert!(err.to_string().contains("port collision"));

    let port_oob = merge_obj(minimal_config("./keys"), json!({"rpc_port": 70000}));
    let err = validate_config(&port_oob).expect_err("port out of bounds");
    assert!(err.to_string().contains("outside 1..65535"));

    let dcap_no_measurements = merge_obj(
        minimal_config("./keys"),
        json!({"tee": {"mode": "dcap-required"}}),
    );
    let err = run_tee_stage(
        "/nonexistent",
        Path::new("/nonexistent"),
        Path::new("/nonexistent"),
        &dcap_no_measurements,
    )
    .expect_err("dcap without measurements");
    assert!(err.to_string().contains("mrenclave"));
}

#[test]
fn key_derivation_and_discovery_checks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let vectors = [
        (1_u32, "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"),
        (2_u32, "0x2b5ad5c4795c026514f8317c7a215e218dccd6cf"),
    ];
    for (scalar, expected) in vectors {
        let path = tmp.path().join(format!("key-{scalar}.hex"));
        fs::write(&path, format!("{scalar:064x}\n")).expect("write key");
        assert_eq!(
            evm_address_from_key_file(&path).expect("derive evm address"),
            expected
        );
    }

    let short_key = tmp.path().join("evm-key.hex");
    fs::write(&short_key, "dead\n").expect("write short key");
    let err = evm_address_from_key_file(&short_key).expect_err("short key");
    assert!(err.to_string().contains("32-byte"));

    let node_id: Vec<u8> = (0..32u8).collect();
    let rad_pub = tmp.path().join("radicle.pub");
    write_ssh_ed25519_pub(&rad_pub, &node_id);
    assert_eq!(
        radicle_node_id_from_public_key(&rad_pub).expect("radicle node id"),
        hex::encode(&node_id)
    );

    fs::write(&rad_pub, "ssh-rsa AAAA outbe\n").expect("write rsa");
    let err = radicle_node_id_from_public_key(&rad_pub).expect_err("wrong algo");
    assert!(err.to_string().contains("ssh-ed25519"));

    let keys_dir = tmp.path().join("keys");
    fs::create_dir_all(&keys_dir).expect("mkdir keys");
    let cfg = minimal_config(&keys_dir.display().to_string());
    let err = discover_validators(&cfg, &keys_dir, "/nonexistent").expect_err("missing dir");
    assert!(err.to_string().contains("missing key directory"));

    fs::create_dir_all(keys_dir.join("validator-0")).expect("mkdir validator-0");
    let err = discover_validators(&cfg, &keys_dir, "/nonexistent").expect_err("missing key file");
    assert!(err.to_string().contains("signing-key.hex"));
}

#[test]
fn seed_merge_and_baseline_profile_checks() {
    let base = load_yaml(&base_profile_path()).expect("load baseline");
    validate_config(&base).expect("validate baseline");
    let base_keys: BTreeSet<&str> = base
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    let allowed_top = top_level_keys();
    let diff: Vec<&str> = base_keys.difference(&allowed_top).copied().collect();
    assert!(diff.is_empty());

    let seed = build_seed(&json!({"staking": {"min_stake": "7"}})).expect("build_seed");
    assert_eq!(seed["staking"]["min_stake"], json!("7"));
    assert_eq!(
        seed["staking"]["unbonding_period"],
        base["staking"]["unbonding_period"]
    );

    let pairs = json!([{"base": "COEN", "quote": "978", "initial_rate": "2"}]);
    let seed2 = build_seed(&json!({"oracle": {"pairs": pairs}})).expect("build_seed pairs");
    assert_eq!(seed2["oracle"]["pairs"], pairs);
    assert_eq!(seed2["oracle"]["config"], base["oracle"]["config"]);

    let default_seed = build_seed(&json!({})).expect("default build_seed");
    assert!(base.get("metadosis").is_none());
    assert!(default_seed.get("metadosis").is_none());
    for section in SEED_SECTIONS {
        if let Some(expected) = base.get(section) {
            assert_eq!(&default_seed[section], expected);
        }
    }

    let mut base_without_feeder = base.clone();
    base_without_feeder
        .as_object_mut()
        .expect("obj")
        .remove("feeder_pairs");
    base_without_feeder
        .as_object_mut()
        .expect("obj")
        .remove("feeder_seeds");
    assert_eq!(
        build_seed(&base_without_feeder).expect("fixed point"),
        default_seed
    );
}

#[test]
fn example_file_parses_validates_and_matches_baseline() {
    let example_path: PathBuf = profiles_dir().join("network.example.yaml");
    let config = load_yaml(&example_path).expect("load example");
    validate_config(&config).expect("validate example");
    assert_eq!(config["validators"].as_array().expect("arr").len(), 4);
    assert_eq!(config["keys_dir"], json!("./keys"));

    let mut uncommented_lines = Vec::new();
    for line in fs::read_to_string(&example_path)
        .expect("read example")
        .lines()
    {
        let stripped = line.trim_start();
        let indent = &line[..line.len() - stripped.len()];
        if stripped.starts_with('#') && !stripped.starts_with("# -") {
            let candidate = stripped[1..].strip_prefix(' ').unwrap_or(&stripped[1..]);
            let token = candidate.trim_start().split(' ').next().unwrap_or("");
            if token.ends_with(':') || candidate.trim_start().starts_with("- ") {
                let without_comment = candidate
                    .split("  #")
                    .next()
                    .unwrap_or(candidate)
                    .trim_end();
                uncommented_lines.push(format!("{indent}{without_comment}"));
                continue;
            }
        }
        if !stripped.starts_with('#') {
            let without_comment = line.split("  #").next().unwrap_or(line).trim_end();
            uncommented_lines.push(without_comment.to_owned());
        }
    }
    let uncommented_yaml = format!("{}\n", uncommented_lines.join("\n"));
    let uncommented =
        parse_yaml_str(&example_path, &uncommented_yaml).expect("parse uncommented example");
    let base = load_yaml(&base_profile_path()).expect("load base");
    for section in [
        "staking",
        "rewards",
        "validator_set",
        "code_registry",
        "intex_profile",
        "oracle",
    ] {
        assert_eq!(
            uncommented[section], base[section],
            "example default for `{section}` differs from the baseline seed"
        );
    }
    assert_eq!(uncommented["gas_limit"], json!(DEFAULT_GAS_LIMIT));
    assert_eq!(
        uncommented["epoch_length_blocks"],
        json!(DEFAULT_EPOCH_LENGTH_BLOCKS)
    );
    assert_eq!(
        parse_u256(&uncommented["prefund_coen_units"]).expect("prefund"),
        U256::from(DEFAULT_PREFUND_COEN_UNITS)
    );
    assert_eq!(
        uncommented["consensus_p2p_port"],
        json!(DEFAULT_CONSENSUS_P2P_PORT)
    );
}

#[test]
fn seed_stage_end_to_end_and_reproducibility() {
    let feeder_cfg = json!({
        "feeder_pairs": "testnet/pairs.toml",
        "feeder_seeds": "testnet/seeds.yaml",
    });
    let oracle = &build_seed(&feeder_cfg).expect("build_seed")["oracle"];
    let mut rated: BTreeSet<String> = BTreeSet::from(["840".to_owned()]);
    for rate in oracle["initial_rates"].as_array().expect("rates") {
        rated.insert(rate["quote"].as_str().expect("quote").to_owned());
    }
    let seeded_quotes: BTreeSet<String> = oracle["scurve_seeds"]
        .as_array()
        .expect("scurves")
        .iter()
        .map(|s| s["pair_quote"].as_str().expect("pair_quote").to_owned())
        .collect();
    let every: BTreeSet<String> = ["840", "978", "392", "826", "156", "344"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(rated, every);
    assert_eq!(seeded_quotes, every);

    let tmp = tempfile::tempdir().expect("tempdir");
    let tmp_str = tmp.path().display().to_string();
    let seeded_feeder = seed_once(tmp.path(), &merge_obj(minimal_config(&tmp_str), feeder_cfg));
    assert_eq!(
        seeded_feeder["alloc"][ORACLE_ADDRESS]["storage"][hex32(34u64)],
        json!(hex32(6u64))
    );

    let prefund_cfg = merge_obj(
        minimal_config(&tmp_str),
        json!({
            "prefund_coen_units": 5_000_000,
            "nod_profile": {"profile": "dev"},
            "credis_profile": {"profile": "dev"},
        }),
    );
    let seeded = seed_once(tmp.path(), &prefund_cfg);
    assert_eq!(seeded["config"]["chainId"], json!(424242));
    assert_eq!(seeded["config"]["epochLengthBlocks"], json!(300));
    let alloc = &seeded["alloc"];
    for validator in fake_validators() {
        let addr = validator["address"].as_str().expect("addr");
        assert_eq!(alloc[addr]["balance"], json!(format!("0x{:x}", 5_000_000)));
    }
    assert_eq!(
        alloc[VALIDATOR_SET_ADDRESS]["storage"][hex32(20u64)],
        json!(hex32(4u64))
    );
    assert_eq!(
        alloc[NOD_ADDRESS]["storage"][hex32(NOD_PROFILE_SLOT)],
        json!(hex32(1u64))
    );
    assert_eq!(
        alloc[CREDIS_ADDRESS]["storage"][hex32(CREDIS_PROFILE_SLOT)],
        json!(hex32(1u64))
    );

    // VaultRouter routes check
    let vr_storage = alloc[VAULT_ROUTER_ADDRESS]["storage"]
        .as_object()
        .expect("vr storage");
    for slot in (0..=3u64).chain(10..=32u64) {
        assert!(!vr_storage.contains_key(&hex32(slot)));
    }
    assert_eq!(vr_storage[&hex32(4u64)], json!(hex32(4u64)));
    assert_eq!(vr_storage[&hex32(7u64)], json!(hex32(1u64)));
    let pos_key = mapping_key(
        &address_bytes("0000000000000000000000000000000000001015").expect("addr"),
        5,
    );
    assert_eq!(vr_storage[&pos_key], json!(hex32(1u64)));
    let val_key = hex32(data_slot(4u64));
    assert_eq!(
        vr_storage[&val_key],
        json!(hex32(
            address_as_u256("0000000000000000000000000000000000001015").expect("u256")
        ))
    );

    // Unknown and legacy seed keys check
    let mut g = json!({"config": {"chainId": 1}, "alloc": {}, "timestamp": "0x0"});
    let err = apply_seed(&mut g, &json!({"zeta": {}, "alpha": {}}), None, None)
        .expect_err("unknown keys");
    let mut allowed: BTreeSet<&str> = SEED_SECTIONS.iter().copied().collect();
    allowed.insert("hyperlane_controller");
    let allowed_vec: Vec<&str> = allowed.into_iter().collect();
    assert_eq!(
        err.to_string(),
        format!(
            "unknown key(s): alpha, zeta; allowed: {}",
            allowed_vec.join(", ")
        )
    );
    let err = apply_seed(
        &mut g,
        &json!({"intex_factory": {}, "vault_routr": {}}),
        None,
        None,
    )
    .expect_err("intex_factory");
    assert!(err.to_string().contains("renamed to intex_profile"));
    let err = apply_seed(&mut g, &json!({"gems": [], "vault_routr": {}}), None, None)
        .expect_err("gems unsupported");
    assert!(err.to_string().contains("gems is no longer supported"));

    // Mainnet profile seed check
    let mainnet_cfg = merge_obj(
        minimal_config(&tmp_str),
        json!({
            "network": "mainnet",
            "chain_id": 676,
            "tee": {"mode": "dcap-required"},
            "enclave_image": format!("outbe-tee-enclave@sha256:{}", "ab".repeat(32)),
            "price_feed_rest": "https://prices.outbe.net",
        }),
    );
    let mainnet_seeded = seed_once(tmp.path(), &mainnet_cfg);
    assert_eq!(mainnet_seeded["config"]["chainId"], json!(676));
    assert!(!mainnet_seeded["alloc"][REWARDS_ADDRESS]["storage"]
        .as_object()
        .expect("rewards")
        .is_empty());

    // Reproducibility and stale timestamp check
    let first_dir = tempfile::tempdir().expect("first");
    let second_dir = tempfile::tempdir().expect("second");
    let pinned_cfg = merge_obj(
        minimal_config(&first_dir.path().display().to_string()),
        json!({
            "timestamp": 1_756_032_000,
            "allow_stale_timestamp": true,
        }),
    );
    let a = seed_once(first_dir.path(), &pinned_cfg);
    let b = seed_once(second_dir.path(), &pinned_cfg);
    assert_eq!(a, b);
    assert_eq!(a["timestamp"], json!(format!("0x{:x}", 1_756_032_000_u64)));
    assert_eq!(a["config"]["genesisTime"], json!("2025-08-24T10:40:00Z"));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_secs();
    let stale = now - 48 * 3600;
    let err = build_base_genesis(&merge_obj(
        minimal_config("./keys"),
        json!({"timestamp": stale}),
    ))
    .expect_err("stale timestamp");
    assert!(err.to_string().contains("lease is already expired"));

    // Oracle pair scale check
    let token = "0x1111111111111111111111111111111111111111";
    let rate_big = (U256::from(65_000u64) * U256::from(10u64).pow(U256::from(18u64))).to_string();
    let oracle_pairs_cfg = merge_obj(
        minimal_config(&tmp_str),
        json!({
            "oracle": {
                "pairs": [
                    {"base": "COEN", "quote": "840", "initial_rate": "1250000"},
                    {"base": token, "quote": "840", "initial_rate": rate_big},
                ]
            }
        }),
    );
    let seeded_oracle = seed_once(tmp.path(), &oracle_pairs_cfg);
    let oracle_storage = &seeded_oracle["alloc"][ORACLE_ADDRESS]["storage"];
    let pair2_slot = U256::from_str_radix(
        mapping_key(&u32_bytes(2), 43)
            .strip_prefix("0x")
            .expect("0x"),
        16,
    )
    .expect("u256");
    assert_eq!(
        oracle_storage[hex32(pair2_slot)],
        json!(hex32(U256::from_be_slice(
            &asset_address(token).expect("addr")
        )))
    );
}
