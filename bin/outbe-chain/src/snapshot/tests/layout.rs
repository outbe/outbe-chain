use std::{ffi::OsString, fs, path::Path};

use super::super::config::{parse_node_inputs, resolve_layout};

fn native_arguments(root: &Path) -> Vec<OsString> {
    let genesis = root.join("genesis.json");
    crate::tee_genesis::run(&[
        "outbe-chain".into(),
        "tee".into(),
        "genesis".into(),
        "--input".into(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/genesis-final.json"
        )
        .into(),
        "--output".into(),
        genesis.to_str().unwrap().into(),
        "--mode".into(),
        "gramine-direct-dev".into(),
    ])
    .unwrap();
    fs::create_dir(root.join("configuration")).unwrap();
    let storage = root.join("configuration/offchain.toml");
    fs::write(&storage, "version = 1\nbackend = 'rocksdb'\nstart_block = 17\n[rocksdb]\npath = '../projection'\nsecondary_path = '../secondary'\n").unwrap();
    vec![
        "--chain".into(),
        genesis.into_os_string(),
        "--datadir".into(),
        root.join("chain").into_os_string(),
        "--projection.storage-config".into(),
        storage.into_os_string(),
    ]
}

#[test]
fn native_defaults_and_toml_relative_primary_resolve_without_creating_stores() {
    let root = tempfile::tempdir().unwrap();
    let arguments = native_arguments(root.path());
    let inputs = parse_node_inputs(arguments).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    let d = root.path().join("chain");
    assert_eq!(layout.chain_root, d);
    assert_eq!(layout.consensus_root, d.join("consensus"));
    assert_eq!(layout.ocomp_root, root.path().join("ocomp/domain-v1"));
    assert_eq!(layout.offchain_root, root.path().join("projection"));
    assert_eq!(layout.static_files_root, d.join("static_files"));
    assert_eq!(layout.execution_rocksdb_root, d.join("rocksdb"));
    assert_eq!(layout.projection_start_block, 17);
    assert_eq!(layout.chain.chain().id(), 54322345);
    for protected in [
        root.path().join("genesis.json"),
        d.join("keys"),
        d.join("discovery-secret"),
        d.join("jwt.hex"),
        d.join("reth.toml"),
        root.path().join("configuration/offchain.toml"),
    ] {
        assert!(
            layout.protected.0.contains(&protected),
            "{}",
            protected.display()
        );
    }
    assert!(!d.exists());
    assert!(!layout.ocomp_root.exists());
    assert!(!layout.offchain_root.exists());
    assert!(!root.path().join("secondary").exists());
}

#[test]
fn native_overrides_and_effective_validator_key_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let mut arguments = native_arguments(root.path());
    arguments.push("--validator".into());
    for (flag, name) in [
        ("--consensus.storage-dir", "consensus"),
        ("--consensus.keys-dir", "keys"),
        ("--consensus.signing-key", "signer/bls.hex"),
        ("--consensus.signing-share", "keys/share.hex"),
        ("--consensus.public-polynomial", "keys/poly.hex"),
        ("--consensus.dkg-output", "keys/output.hex"),
        ("--datadir.static-files", "static"),
        ("--datadir.rocksdb", "execution-rocks"),
        ("--p2p-secret-key", "p2p.hex"),
        ("--authrpc.jwtsecret", "jwt.hex"),
        ("--config", "reth.toml"),
    ] {
        arguments.extend([flag.into(), root.path().join(name).into_os_string()]);
    }
    let inputs = parse_node_inputs(arguments).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    assert_eq!(layout.consensus_root, root.path().join("consensus"));
    assert_eq!(layout.static_files_root, root.path().join("static"));
    assert_eq!(
        layout.execution_rocksdb_root,
        root.path().join("execution-rocks")
    );
    for name in [
        "keys",
        "signer/bls.hex",
        "signer/evm-key.hex",
        "keys/share.hex",
        "keys/poly.hex",
        "keys/output.hex",
        "p2p.hex",
        "jwt.hex",
        "reth.toml",
    ] {
        assert!(
            layout.protected.0.contains(&root.path().join(name)),
            "{name}"
        );
    }
    assert!(!root.path().join("consensus").exists());
    assert!(!root.path().join("keys").exists());
}

#[test]
fn mongo_is_reported_as_unsupported_without_connecting() {
    let root = tempfile::tempdir().unwrap();
    let arguments = native_arguments(root.path());
    fs::write(root.path().join("configuration/offchain.toml"), "version = 1\nbackend = 'mongodb'\n[mongodb]\nuri = 'mongodb://127.0.0.1:1'\ndatabase = 'snapshot'\n").unwrap();
    let inputs = parse_node_inputs(arguments).unwrap();
    assert!(resolve_layout(&inputs)
        .unwrap_err()
        .to_string()
        .contains("RocksDB"));
    assert!(!root.path().join("chain").exists());
}

#[test]
fn inline_genesis_is_configuration_data_not_a_protected_file_path() {
    let root = tempfile::tempdir().unwrap();
    let mut arguments = native_arguments(root.path());
    let inline = fs::read_to_string(root.path().join("genesis.json")).unwrap();
    arguments[1] = inline.clone().into();
    let inputs = parse_node_inputs(arguments).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    assert!(!layout
        .protected
        .0
        .contains(&std::path::PathBuf::from(inline)));
    assert_eq!(layout.chain.chain().id(), 54322345);
    assert!(!layout.chain_root.exists());
}
