use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use super::*;

fn sample_metadata_json(repo_root: &Path, dep_kind_field: &str) -> String {
    let bc_manifest = repo_root.join("crates/blockchain/primitives/Cargo.toml");
    let bc_peer = repo_root.join("crates/blockchain/tee/Cargo.toml");
    let core_manifest = repo_root.join("crates/core/enclave/Cargo.toml");
    let kind_value = if dep_kind_field == "normal" {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(dep_kind_field.to_owned())
    };
    serde_json::json!({
        "packages": [
            {
                "name": "outbe-primitives",
                "manifest_path": bc_manifest,
                "dependencies": [
                    { "name": "alloy-primitives", "kind": null }
                ]
            },
            {
                "name": "outbe-tee",
                "manifest_path": bc_peer,
                "dependencies": [
                    { "name": "outbe-primitives", "kind": null },
                    { "name": "outbe-enclave", "kind": kind_value }
                ]
            },
            {
                "name": "outbe-enclave",
                "manifest_path": core_manifest,
                "dependencies": []
            }
        ]
    })
    .to_string()
}

#[test]
fn metadata_inspector_rejects_normal_build_and_dev_edges_outside_blockchain() {
    let temp = tempfile::tempdir().unwrap();
    let root = canonicalize_path(temp.path()).unwrap();

    for (raw_kind, expected_kind) in [
        ("normal", DependencyEdgeKind::Normal),
        ("build", DependencyEdgeKind::Build),
        ("dev", DependencyEdgeKind::Dev),
    ] {
        let json = sample_metadata_json(&root, raw_kind);
        let (_, violations) = inspect_metadata_edges(&root, &json).unwrap();
        assert_eq!(
            violations,
            vec![LayerViolation::ForbiddenManifestDependency {
                source_package: "outbe-tee".to_owned(),
                kind: expected_kind,
                target_package: "outbe-enclave".to_owned(),
                target_manifest: root.join("crates/core/enclave/Cargo.toml"),
            }]
        );
    }
}

#[test]
fn resolved_tree_inspector_rejects_outside_workspace_packages() {
    let mut outside = BTreeMap::new();
    outside.insert(
        "outbe-poseidon".to_owned(),
        PathBuf::from("/repo/crates/zk/poseidon/Cargo.toml"),
    );
    let tree = "\
outbe-primitives v0.1.0 (/repo/crates/blockchain/primitives)
alloy-primitives v1.0.0
outbe-poseidon v0.1.0 (/repo/crates/zk/poseidon)
";
    assert_eq!(
        find_resolved_tree_violations(
            (Path::new("/repo"), Path::new("/repo/crates/blockchain")),
            tree,
            &outside,
        ),
        vec![LayerViolation::ForbiddenResolvedTreeEdge {
            target_package: "outbe-poseidon".to_owned(),
        }]
    );
}

#[test]
fn source_scanner_allows_internal_paths_and_two_readonly_release_pins_only() {
    let temp = tempfile::tempdir().unwrap();
    let root = canonicalize_path(temp.path()).unwrap();
    let tee_dir = root.join("crates/blockchain/tee");
    fs::create_dir_all(tee_dir.join("src")).unwrap();
    fs::create_dir_all(tee_dir.join("tests/fixtures")).unwrap();
    fs::create_dir_all(root.join("release")).unwrap();
    fs::create_dir_all(root.join("crates/core/enclave/src")).unwrap();

    fs::write(tee_dir.join("Cargo.toml"), "[package]\nname = \"tee\"\n").unwrap();
    fs::write(tee_dir.join("src/helper.rs"), "pub const X: u8 = 1;\n").unwrap();
    fs::write(tee_dir.join("tests/fixtures/quote.bin"), b"quote").unwrap();
    fs::write(root.join("release/project-toolchain-v1.json"), "{}").unwrap();
    fs::write(root.join("release/dcap-native-qvl-v1.json"), "{}").unwrap();
    fs::write(root.join("release/other.json"), "{}").unwrap();
    fs::write(root.join("crates/core/enclave/src/lib.rs"), "// outside\n").unwrap();

    let valid_source = r#"
        // #[path = "../../../../crates/core/enclave/src/lib.rs"]
        const QUOTE_CHAR: char = '"';
        const LITERAL: &str = "include_str!(\"../../../../release/other.json\")";
        #[derive(Debug)]
        #[path = "helper.rs"]
        mod helper;
        const TOOLCHAIN: &str = include_str!("../../../../release/project-toolchain-v1.json");
        const QVL: &str = include_str!("../../../../release/dcap-native-qvl-v1.json");
        const FIXTURE: &[u8] = include_bytes!(
            "../tests/fixtures/quote.bin"
        );
    "#;
    fs::write(tee_dir.join("src/lib.rs"), valid_source).unwrap();

    let (files, inclusions, violations) =
        scan_blockchain_sources(&root, &root.join(BLOCKCHAIN_REL_DIR)).unwrap();
    assert_eq!(files, 2);
    assert_eq!(inclusions, 4);
    assert!(
        violations.is_empty(),
        "unexpected violations: {violations:?}"
    );

    let bad_source = r#"
        #[cfg_attr(test, path = "../../../../crates/core/enclave/src/lib.rs")]
        mod leaked_code;
        #[path = "../../../../release/project-toolchain-v1.json"]
        mod code_from_json;
        const OTHER_PIN: &str = include_str!("../../../../release/other.json");
        const ESCAPED_MANIFEST: &[u8] = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../core/enclave/src/lib.rs"
        ));
    "#;
    fs::write(tee_dir.join("src/lib.rs"), bad_source).unwrap();

    let (_, _, bad_violations) =
        scan_blockchain_sources(&root, &root.join(BLOCKCHAIN_REL_DIR)).unwrap();
    assert_eq!(bad_violations.len(), 4, "got: {bad_violations:?}");
}

#[test]
fn cargo_tree_args_and_inspector_cover_all_features_and_dev_build_edges() {
    let pkgs = BTreeSet::from(["outbe-primitives".to_owned(), "outbe-tee".to_owned()]);
    assert_eq!(
        build_cargo_tree_args(&pkgs),
        vec![
            "tree",
            "--locked",
            "--all-features",
            "-e",
            "features,normal,build,dev",
            "--prefix",
            "none",
            "-p",
            "outbe-primitives",
            "-p",
            "outbe-tee",
        ]
    );

    let outside = BTreeMap::from([
        (
            "outbe-zerofee".to_owned(),
            PathBuf::from("/repo/crates/system/zerofee/Cargo.toml"),
        ),
        (
            "outbe-enclave".to_owned(),
            PathBuf::from("/repo/crates/core/enclave/Cargo.toml"),
        ),
    ]);
    let tree = "\
outbe-tee feature \"test-utils\" (command-line)
outbe-tee v0.1.0 (/repo/crates/blockchain/tee)
excluded-bc-helper v0.1.0 (/repo/crates/blockchain/excluded-helper)
secret-domain v0.1.0 (/repo/excluded/secret-domain)
radicle-cob v0.17.0 (/repo/vendor/heartwood/crates/radicle-cob)
outbe-zerofee feature \"default\"
outbe-zerofee v0.1.0 (/repo/crates/system/zerofee)
outbe-enclave v0.1.0 (/repo/crates/core/enclave)
";
    assert_eq!(
        find_resolved_tree_violations(
            (Path::new("/repo"), Path::new("/repo/crates/blockchain")),
            tree,
            &outside,
        ),
        vec![
            LayerViolation::ForbiddenResolvedTreeEdge {
                target_package: "outbe-enclave".to_owned(),
            },
            LayerViolation::ForbiddenResolvedTreeEdge {
                target_package: "outbe-zerofee".to_owned(),
            },
            LayerViolation::ForbiddenResolvedTreeEdge {
                target_package: "secret-domain".to_owned(),
            },
        ]
    );
}

#[test]
fn local_path_inspector_allows_blockchain_and_pinned_vendors_and_rejects_excluded_dirs() {
    let temp = tempfile::tempdir().unwrap();
    let root = canonicalize_path(temp.path()).unwrap();
    for rel in [
        "crates/blockchain/radicle",
        "crates/blockchain/primitives",
        "vendor/alloy-evm",
        "vendor/heartwood/crates/radicle",
        "vendor/heartwood/crates/radicle-crypto",
        "vendor/heartwood/crates/radicle-git-metadata",
        "vendor/heartwood/crates/radicle-node",
        "excluded/secret-domain",
    ] {
        fs::create_dir_all(root.join(rel)).unwrap();
    }

    let allowed_json = serde_json::json!({
        "packages": [{
            "name": "outbe-radicle",
            "manifest_path": root.join("crates/blockchain/radicle/Cargo.toml"),
            "dependencies": [
                { "name": "outbe-primitives", "kind": null, "path": root.join("crates/blockchain/primitives") },
                { "name": "alloy-evm", "kind": null, "path": root.join("vendor/alloy-evm") },
                { "name": "radicle", "kind": null, "path": root.join("vendor/heartwood/crates/radicle") },
                { "name": "radicle-crypto", "kind": null, "path": root.join("vendor/heartwood/crates/radicle-crypto") },
                { "name": "radicle-git-metadata", "kind": "dev", "path": root.join("vendor/heartwood/crates/radicle-git-metadata") }
            ]
        }]
    })
    .to_string();
    let (_, allowed_violations) = inspect_metadata_edges(&root, &allowed_json).unwrap();
    assert!(
        allowed_violations.is_empty(),
        "unexpected violations: {allowed_violations:?}"
    );

    let rejected_json = serde_json::json!({
        "packages": [{
            "name": "outbe-radicle",
            "manifest_path": root.join("crates/blockchain/radicle/Cargo.toml"),
            "dependencies": [
                { "name": "secret-domain", "kind": "dev", "path": root.join("excluded/secret-domain") },
                { "name": "radicle-node", "kind": "build", "path": root.join("vendor/heartwood/crates/radicle-node") }
            ]
        }]
    })
    .to_string();
    let (_, rejected_violations) = inspect_metadata_edges(&root, &rejected_json).unwrap();
    assert_eq!(
        rejected_violations,
        vec![
            LayerViolation::ForbiddenLocalPathDependency {
                source_package: "outbe-radicle".to_owned(),
                kind: DependencyEdgeKind::Dev,
                target_package: "secret-domain".to_owned(),
                resolved_path: root.join("excluded/secret-domain"),
            },
            LayerViolation::ForbiddenLocalPathDependency {
                source_package: "outbe-radicle".to_owned(),
                kind: DependencyEdgeKind::Build,
                target_package: "radicle-node".to_owned(),
                resolved_path: root.join("vendor/heartwood/crates/radicle-node"),
            },
        ]
    );
}

#[test]
fn build_and_tests_scanner_rejects_cross_layer_path_string_literals() {
    let temp = tempfile::tempdir().unwrap();
    let root = canonicalize_path(temp.path()).unwrap();
    let tee_dir = root.join("crates/blockchain/tee");
    fs::create_dir_all(tee_dir.join("tests")).unwrap();
    fs::create_dir_all(root.join("release")).unwrap();
    fs::write(tee_dir.join("Cargo.toml"), "[package]\nname = \"tee\"\n").unwrap();
    fs::write(root.join("release/project-toolchain-v1.json"), "{}").unwrap();

    fs::write(
        tee_dir.join("build.rs"),
        "fn main() {\n    println!(\"cargo:rerun-if-changed=../../../release/project-toolchain-v1.json\");\n}\n",
    )
    .unwrap();
    fs::write(
        tee_dir.join("tests/smoke.rs"),
        "const LOCAL_REL: &str = \"../other/a.dat\";\n",
    )
    .unwrap();
    let (_, _, clean_violations) =
        scan_blockchain_sources(&root, &root.join(BLOCKCHAIN_REL_DIR)).unwrap();
    assert!(
        clean_violations.is_empty(),
        "unexpected violations: {clean_violations:?}"
    );

    fs::write(
        tee_dir.join("build.rs"),
        "fn main() {\n    let _ = std::path::Path::new(\"../../contracts/precompiles/src/Foo.sol\");\n    let _ = std::path::Path::new(\"./../../system/common/src/addresses.rs\");\n}\n",
    )
    .unwrap();
    fs::write(
        tee_dir.join("tests/smoke.rs"),
        "const CORE: &str = \"crates/core/enclave/src/lib.rs\";\nconst BIN: &str = \"bin/outbe-chain/src/main.rs\";\n",
    )
    .unwrap();
    let (_, _, violations) =
        scan_blockchain_sources(&root, &root.join(BLOCKCHAIN_REL_DIR)).unwrap();
    assert_eq!(
        violations,
        vec![
            LayerViolation::ForbiddenCrossLayerPathLiteral {
                source_file: tee_dir.join("build.rs"),
                line: 2,
                literal: "../../contracts/precompiles/src/Foo.sol".to_owned(),
            },
            LayerViolation::ForbiddenCrossLayerPathLiteral {
                source_file: tee_dir.join("build.rs"),
                line: 3,
                literal: "./../../system/common/src/addresses.rs".to_owned(),
            },
            LayerViolation::ForbiddenCrossLayerPathLiteral {
                source_file: tee_dir.join("tests/smoke.rs"),
                line: 1,
                literal: "crates/core/enclave/src/lib.rs".to_owned(),
            },
            LayerViolation::ForbiddenCrossLayerPathLiteral {
                source_file: tee_dir.join("tests/smoke.rs"),
                line: 2,
                literal: "bin/outbe-chain/src/main.rs".to_owned(),
            },
        ]
    );
}
