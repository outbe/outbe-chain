//! Cross-language OGD1 bytes, actual seeding, and opt-in DKG window checks.

use std::fs;
use std::path::PathBuf;

use alloy_primitives::U256;
use outbe_genesis_cli::network::repo_root;
use outbe_genesis_cli::seed_alloc::{seed_code_registry, seed_governments_descriptor};
use outbe_genesis_cli::seed_codec::{CODE_REGISTRY_ADDRESS, StorageBuilder, data_slot, hex32};
use outbe_genesis_cli::seed_genesis::apply_seed;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn base_genesis() -> Value {
    json!({
        "config": {"chainId": 2026},
        "timestamp": "0x6553f100",
        "alloc": {}
    })
}

fn configuration() -> Value {
    json!({
        "max_repositories": 8,
        "governments": {
            "description": "Network governance",
            "initial_tree_oid": "0x4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "initial_content_manifest_sha256": "0x4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
        }
    })
}

#[test]
fn seeded_words_match_p0_descriptor_and_rust_storage_fixture() {
    let mut base = base_genesis();
    let mut storage = StorageBuilder::new();
    let cfg = configuration();
    seed_code_registry(&mut storage, &cfg).expect("seed_code_registry");
    seed_governments_descriptor(&mut base, &mut storage, &cfg)
        .expect("seed_governments_descriptor");

    let desc_hex = base["config"]["radicleGovernments"]
        .as_str()
        .expect("radicleGovernments str")
        .strip_prefix("0x")
        .expect("0x prefix");
    let descriptor = hex::decode(desc_hex).expect("hex decode descriptor");
    let digest = hex::encode(Sha256::digest(&descriptor));
    assert_eq!(
        digest,
        "0349a0178f4f477562b46c3c9690408ff65cdeb41d059898d38b85791fa92bcf"
    );
    assert_eq!(storage.entries[&hex32(10u64)], format!("0x{digest}"));
    let len_marker = usize::from_str_radix(
        storage.entries[&hex32(11u64)]
            .strip_prefix("0x")
            .expect("0x prefix"),
        16,
    )
    .expect("hex usize");
    assert_eq!(len_marker, descriptor.len() * 2 + 1);

    let mut stored = Vec::new();
    let word_count = descriptor.len().div_ceil(32);
    for i in 0..word_count {
        let word_slot = data_slot(11u64) + U256::from(i);
        let word_hex = storage.entries[&hex32(word_slot)]
            .strip_prefix("0x")
            .expect("0x prefix");
        stored.extend_from_slice(&hex::decode(word_hex).expect("word bytes"));
    }
    assert_eq!(&stored[..descriptor.len()], descriptor.as_slice());

    let fixture_path: PathBuf = repo_root()
        .join("crates/system/coderegistry/tests/fixtures/governments-genesis-storage.json");
    let fixture: Value =
        serde_json::from_str(&fs::read_to_string(fixture_path).expect("read fixture"))
            .expect("parse fixture");
    let actual = json!({
        "config": base["config"],
        "timestamp": base["timestamp"],
        "storage": storage.entries,
    });
    assert_eq!(actual, fixture);
}

#[test]
fn real_seed_pipeline_is_opt_in_and_commits_descriptor() {
    let mut g_enabled = base_genesis();
    let enabled = apply_seed(
        &mut g_enabled,
        &json!({"code_registry": configuration()}),
        None,
        None,
    )
    .expect("apply_seed enabled");

    let mut g_disabled = base_genesis();
    let disabled = apply_seed(
        &mut g_disabled,
        &json!({"code_registry": {"max_repositories": 8}}),
        None,
        None,
    )
    .expect("apply_seed disabled");

    let account = &enabled["alloc"][CODE_REGISTRY_ADDRESS];
    assert_eq!(account["code"], "0xef");
    assert!(account["storage"].get(hex32(10u64)).is_some());
    assert!(enabled["config"].get("radicleGovernments").is_some());
    assert!(disabled["config"].get("radicleGovernments").is_none());
    assert_eq!(
        disabled["alloc"][CODE_REGISTRY_ADDRESS]["storage"],
        json!({hex32(5u64): hex32(8u64)})
    );
}

#[test]
fn dkg_window_uses_effective_prepare_and_preserves_legacy() {
    let cfg = configuration();
    for (epoch, prepare) in [(300_i64, 30_i64), (65, 1), (64, 0)] {
        let mut base = base_genesis();
        base["config"]["epochLengthBlocks"] = json!(epoch);
        base["config"]["dkgPrepareWindowBlocks"] = json!(prepare);
        seed_governments_descriptor(&mut base, &mut StorageBuilder::new(), &cfg)
            .expect("valid dkg window");
    }

    let disabled_cfg = json!({"max_repositories": 8});
    for (epoch, prepare) in [
        (63_i64, 0_i64),
        (64, 1),
        (65, 2),
        (300, 300),
        (300, 999),
        (300, -1),
    ] {
        let mut base = base_genesis();
        base["config"]["epochLengthBlocks"] = json!(epoch);
        base["config"]["dkgPrepareWindowBlocks"] = json!(prepare);
        assert!(
            seed_governments_descriptor(&mut base, &mut StorageBuilder::new(), &cfg).is_err(),
            "expected error for epoch={epoch}, prepare={prepare}"
        );
        seed_governments_descriptor(&mut base, &mut StorageBuilder::new(), &disabled_cfg)
            .expect("disabled governments should not fail on dkg window");
    }
}

#[test]
fn description_256_byte_boundary_preserves_literal_utf8() {
    let valid_descriptions = [
        String::new(),
        "x".repeat(255),
        "x".repeat(256),
        "я".repeat(128),
        format!("{}x", "я".repeat(127)),
    ];
    for description in valid_descriptions {
        let mut cfg = configuration();
        cfg["governments"]["description"] = json!(description);
        let mut base = base_genesis();
        seed_governments_descriptor(&mut base, &mut StorageBuilder::new(), &cfg)
            .expect("valid description");
        let encoded = hex::decode(
            base["config"]["radicleGovernments"]
                .as_str()
                .expect("str")
                .strip_prefix("0x")
                .expect("0x"),
        )
        .expect("hex");
        let raw = description.as_bytes();
        let encoded_len = u16::from_be_bytes([encoded[41], encoded[42]]) as usize;
        assert_eq!(encoded_len, raw.len());
        assert_eq!(&encoded[43..43 + raw.len()], raw);
        assert_eq!(encoded.len(), 97 + raw.len());
    }

    let invalid_descriptions = [
        "x".repeat(257),
        format!("{}x", "я".repeat(128)),
        "x".repeat(1024),
    ];
    for description in invalid_descriptions {
        let mut cfg = configuration();
        cfg["governments"]["description"] = json!(description);
        let mut base = base_genesis();
        let mut storage = StorageBuilder::new();
        let err = seed_governments_descriptor(&mut base, &mut storage, &cfg)
            .expect_err("expected description length error");
        assert!(err.to_string().contains("256 UTF-8 bytes"));
        assert!(storage.entries.is_empty());
        assert!(base["config"].get("radicleGovernments").is_none());
    }
}

#[test]
fn invalid_descriptor_rejected_before_any_storage_write() {
    let invalid_mutations: &[(&str, Value)] = &[
        ("description", json!("я".repeat(129))),
        ("description", json!("x\r")),
        ("description", json!("x\0")),
        ("initial_tree_oid", json!(format!("0x{}", "00".repeat(20)))),
        (
            "initial_content_manifest_sha256",
            json!(format!("0x{}", "ff".repeat(31))),
        ),
        ("delegates", json!([])),
    ];
    for (key, value) in invalid_mutations {
        let mut cfg = configuration();
        cfg["governments"][*key] = value.clone();
        let mut storage = StorageBuilder::new();
        let mut base = base_genesis();
        assert!(
            seed_governments_descriptor(&mut base, &mut storage, &cfg).is_err(),
            "expected error for {key}"
        );
        assert!(storage.entries.is_empty());
    }

    let mut base = base_genesis();
    base["config"]["radicleGovernments"] = json!("0x00");
    let cfg = configuration();
    let err = seed_governments_descriptor(&mut base, &mut StorageBuilder::new(), &cfg)
        .expect_err("expected conflicting radicleGovernments error");
    assert!(err.to_string().contains("conflicting"));
}
