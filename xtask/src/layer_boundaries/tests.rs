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
        find_resolved_tree_violations(tree, &outside),
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
