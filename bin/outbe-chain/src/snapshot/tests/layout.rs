use std::{ffi::OsString, fs, path::Path};

use super::super::config::{parse_node_inputs, resolve_layout};

#[test]
fn create_cli_requires_signing_key_and_forwards_native_arguments() {
    use crate::cli::snapshot::{SnapshotCli, SnapshotCommand};
    use clap::Parser;
    let error = SnapshotCli::try_parse_from(["snapshot", "create", "--output", "snapshot.tar"])
        .err()
        .unwrap();
    assert!(error.to_string().contains("--signing-key"));
    let parsed = SnapshotCli::try_parse_from([
        "snapshot",
        "create",
        "--output",
        "snapshot.tar",
        "--signing-key",
        "creator.hex",
        "--",
        "--chain",
        "genesis.json",
        "--datadir",
        "chain",
    ])
    .unwrap();
    let SnapshotCommand::Create(args) = parsed.command else {
        panic!("expected create command");
    };
    assert_eq!(
        args.node_args,
        ["--chain", "genesis.json", "--datadir", "chain"].map(std::ffi::OsString::from)
    );
}

pub(super) fn native_arguments(root: &Path) -> Vec<OsString> {
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

#[test]
fn inventory_selects_native_public_domains_and_excludes_signing_authority() {
    use super::super::inventory::enumerate_native_files;
    use outbe_snapshot::manifest::{DomainKind, NativeRoot};
    let root = tempfile::tempdir().unwrap();
    let inputs = parse_node_inputs(native_arguments(root.path())).unwrap();
    let layout = resolve_layout(&inputs).unwrap();
    let job = "11".repeat(32);
    let bundle = "22".repeat(32);
    let mut expected = Vec::new();
    let samples = [
        (NativeRoot::Chain, DomainKind::ExecutionDb, layout.chain_root.clone(), "db/mdbx.dat".to_owned()),
        (NativeRoot::Chain, DomainKind::Ce, layout.chain_root.clone(), "compressed_entities/smt/mdbx.dat".to_owned()),
        (NativeRoot::StaticFiles, DomainKind::StaticFiles, layout.static_files_root.clone(), "header.data".to_owned()),
        (NativeRoot::ExecutionRocksDb, DomainKind::ExecutionRocksDb, layout.execution_rocksdb_root.clone(), "CURRENT".to_owned()),
        (NativeRoot::Offchain, DomainKind::OffchainProjection, layout.offchain_root.clone(), "CURRENT".to_owned()),
        (NativeRoot::Consensus, DomainKind::MarshalBlocks, layout.consensus_root.clone(), "outbe-marshal-blocks-metadata/0".to_owned()),
        (NativeRoot::Consensus, DomainKind::MarshalFinalizations, layout.consensus_root.clone(), "outbe-marshal-finalizations-ordinal/0".to_owned()),
        (NativeRoot::Consensus, DomainKind::MarshalCache, layout.consensus_root.clone(), "outbe-marshal-cache-0/0".to_owned()),
        (NativeRoot::Consensus, DomainKind::MarshalMetadata, layout.consensus_root.clone(), "outbe-marshal-application-metadata/0".to_owned()),
        (NativeRoot::Consensus, DomainKind::ParentCertificates, layout.consensus_root.clone(), "finalized_parent_certs/100".to_owned()),
        (NativeRoot::Consensus, DomainKind::OcompRetention, layout.consensus_root.clone(), "ocomp_retention/pin.v1".to_owned()),
        (NativeRoot::Ocomp, DomainKind::ClosureCheckpoint, layout.ocomp_root.clone(), "exporter-v1/discovery/closure-checkpoint-v1/checkpoint.v1".to_owned()),
        (NativeRoot::Ocomp, DomainKind::Discovery, layout.ocomp_root.clone(), format!("exporter-v1/discovery/{bundle}/pending/{job}.pending")),
        (NativeRoot::Ocomp, DomainKind::ProtocolBundles, layout.ocomp_root.clone(), format!("protocol-bundles-v1/{bundle}.ocb1")),
        (NativeRoot::Ocomp, DomainKind::CasObjects, layout.ocomp_root.clone(), "cas-v1/objects/11/abcdef".to_owned()),
        (NativeRoot::Ocomp, DomainKind::InputReferences, layout.ocomp_root.clone(), format!("exporter-v1/input-refs/{job}/catalog.prepared")),
        (NativeRoot::Ocomp, DomainKind::ExportReceipts, layout.ocomp_root.clone(), format!("exporter-v1/receipts/{job}/receipt.ref")),
        (NativeRoot::Ocomp, DomainKind::ExportBindings, layout.ocomp_root.clone(), format!("supervisor-v1/export-bindings/{job}/binding.lock")),
        (NativeRoot::Ocomp, DomainKind::JobPublicRecords, layout.ocomp_root.clone(), format!("supervisor-v1/jobs/{job}/admissions/catalog.header")),
        (NativeRoot::Ocomp, DomainKind::JobPublicRecords, layout.ocomp_root.clone(), format!("supervisor-v1/jobs/{job}/contributor-payout-v1.bin")),
        (NativeRoot::Ocomp, DomainKind::MaterializationReferences, layout.ocomp_root.clone(), format!("supervisor-v1/materialization-references/{job}/17/{job}.materialization-refs-v1.json")),
        (NativeRoot::Ocomp, DomainKind::MaterializationReferences, layout.ocomp_root.clone(), format!("supervisor-v1/materialization-references/{job}/23/{job}.materialization-refs-v1.tmp")),
        (NativeRoot::Ocomp, DomainKind::MaterializationReferences, layout.ocomp_root.clone(), format!("supervisor-v1/materialization-references/{bundle}/5/{bundle}.materialization-refs-v1.json")),
        (NativeRoot::Ocomp, DomainKind::LocalResults, layout.ocomp_root.clone(), format!("node-v1/local-results/.{job}.pending")),
        (NativeRoot::Ocomp, DomainKind::FatalEvidence, layout.ocomp_root.clone(), "node-v1/fatal-evidence/sticky-fatal-v1".to_owned()),
    ];
    for (native_root, kind, base, name) in &samples {
        let path = base.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, name).unwrap();
        expected.push((*native_root, *kind, path));
    }
    let secrets = [
        layout.chain_root.join("keys/private.hex"),
        layout.consensus_root.join("dkg_share.hex"),
        layout.consensus_root.join("outbe-simplex-1/vote"),
        layout.ocomp_root.join("ocomp-evm-key.hex"),
        layout.ocomp_root.join("ocomp-key-v1.hex"),
        layout.ocomp_root.join("worker-inbox-v1/input"),
        layout.ocomp_root.join("supervisor-v1/sign-once/signed"),
        layout
            .ocomp_root
            .join("supervisor-v1/vote-submissions/signed"),
        layout
            .ocomp_root
            .join("supervisor-v1/materialization-submissions/signed"),
        layout
            .ocomp_root
            .join("supervisor-v1/payout-submissions/signed"),
        layout
            .ocomp_root
            .join(format!("supervisor-v1/jobs/{job}/replay-inbox/input")),
        layout.ocomp_root.join("cas-v1/staging/incomplete"),
    ];
    for path in &secrets {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "private sentinel").unwrap();
    }
    let inventory = enumerate_native_files(&layout).unwrap();
    assert_eq!(inventory.domains.len(), 23);
    for (native_root, kind, path) in expected {
        assert!(
            inventory.domains.iter().any(|domain| domain.kind == kind
                && domain.native_root == native_root
                && domain
                    .members
                    .iter()
                    .any(|member| domain.root.join(member) == path)),
            "{}",
            path.display()
        );
    }
    for path in secrets {
        assert!(
            inventory.domains.iter().all(|domain| domain
                .members
                .iter()
                .all(|member| domain.root.join(member) != path)),
            "{}",
            path.display()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "private sentinel");
    }
    let exex = inventory
        .domains
        .iter()
        .find(|domain| domain.kind == DomainKind::ExexCheckpoint)
        .unwrap();
    assert!(exex.members.is_empty());
    assert!(!layout.ocomp_root.join("node-v1/exex-checkpoint").exists());
    let fatal = layout.ocomp_root.join("node-v1/fatal-evidence");
    fs::remove_file(fatal.join("sticky-fatal-v1")).unwrap();
    fs::remove_dir(&fatal).unwrap();
    let empty = enumerate_native_files(&layout).unwrap();
    assert!(empty
        .domains
        .iter()
        .find(|domain| domain.kind == DomainKind::FatalEvidence)
        .unwrap()
        .members
        .is_empty());
    assert!(!fatal.exists());

    let mut simplex_layout = resolve_layout(&inputs).unwrap();
    simplex_layout.offchain_root = simplex_layout.consensus_root.join("outbe-simplex-1");
    let error = enumerate_native_files(&simplex_layout).unwrap_err();
    assert!(error.to_string().contains("protected"), "{error:#}");

    let mut protected_layout = layout;
    protected_layout
        .protected
        .0
        .push(protected_layout.offchain_root.join("CURRENT"));
    assert!(enumerate_native_files(&protected_layout)
        .unwrap_err()
        .to_string()
        .contains("protected"));
    assert_eq!(
        fs::read_to_string(protected_layout.offchain_root.join("CURRENT")).unwrap(),
        "CURRENT"
    );
}
