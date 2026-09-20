use std::os::unix::fs::symlink;
use std::{fs, path::PathBuf};

use outbe_snapshot::layout::{validate_layout, ProtectedPaths, ResolvedDomain};
use tempfile::TempDir;

fn manifest_json() -> serde_json::Value {
    let block = |number: u64| serde_json::json!({"number": number, "hash": "11".repeat(32)});
    serde_json::json!({
        "version": 1,
        "chain_id": 54322345,
        "genesis_hash": "22".repeat(32),
        "created_at_unix": 1789936000_u64,
        "creator": "fixture operator",
        "source": null,
        "progress": {
            "finalized": block(1000),
            "execution": block(1001),
            "execution_stage": 1001,
            "finish_stage": 1001,
            "partial_state_trie": 1000,
            "unwind": null,
            "storage_version": 2,
            "ce": block(1001),
            "projection": block(1000),
            "ocomp_baseline": block(0),
            "ocomp_previous": block(995),
            "ocomp_current": block(999)
        },
        "domains": [{
            "id": "execution-db",
            "kind": "execution-db",
            "native_root": "chain",
            "native_path": "db",
            "mode": 448,
            "entries": [{
                "path": "mdbx.dat",
                "kind": "file",
                "size": 4,
                "sha256": "33".repeat(32),
                "mode": 384
            }]
        }],
        "file_count": 1,
        "total_bytes": 4
    })
}

#[test]
fn manifest_preserves_distinct_native_frontiers_and_digests_original_bytes() {
    use outbe_snapshot::manifest::{manifest_digest, SnapshotManifestV1};
    let raw = serde_json::to_vec(&manifest_json()).unwrap();
    let manifest = SnapshotManifestV1::from_bytes(&raw).unwrap();
    assert_eq!(manifest.progress.finalized.number, 1000);
    assert_eq!(manifest.progress.execution.number, 1001);
    assert_eq!(manifest.progress.ce.number, 1001);
    assert_eq!(manifest.progress.projection.number, 1000);
    assert_eq!(manifest.progress.ocomp_current.number, 999);
    assert_eq!(manifest.progress.partial_state_trie, Some(1000));
    assert_eq!(serde_json::to_value(&manifest).unwrap(), manifest_json());

    let pretty = serde_json::to_vec_pretty(&manifest_json()).unwrap();
    assert_ne!(manifest_digest(&raw), manifest_digest(&pretty));
    assert_eq!(SnapshotManifestV1::from_bytes(&pretty).unwrap(), manifest);
}

#[test]
fn manifest_rejects_invalid_versions_hashes_and_inconsistent_totals() {
    use outbe_snapshot::manifest::SnapshotManifestV1;
    let reject = |value: serde_json::Value| {
        assert!(
            SnapshotManifestV1::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err(),
            "accepted {value}"
        );
    };
    for (field, bad) in [("version", 2_u64), ("file_count", 2), ("total_bytes", 5)] {
        let mut value = manifest_json();
        value[field] = bad.into();
        reject(value);
    }
    for bad in [
        "",
        "xyz",
        &"AA".repeat(32),
        &format!("0x{}", "11".repeat(32)),
    ] {
        let mut value = manifest_json();
        value["genesis_hash"] = bad.into();
        reject(value);
        let mut value = manifest_json();
        value["domains"][0]["entries"][0]["sha256"] = bad.into();
        reject(value);
    }
    let mut value = manifest_json();
    value["progress"]["storage_version"] = 3.into();
    reject(value);
    let mut value = manifest_json();
    value["domains"][0]["entries"][0]["sha256"] = serde_json::Value::Null;
    reject(value);
}

#[test]
fn manifest_rejects_duplicate_fields_domains_members_and_unknown_data_classes() {
    use outbe_snapshot::manifest::SnapshotManifestV1;
    let raw = serde_json::to_string(&manifest_json()).unwrap();
    assert!(SnapshotManifestV1::from_bytes(
        raw.replace("\"version\":1", "\"version\":1,\"version\":1")
            .as_bytes()
    )
    .is_err());
    let mut value = manifest_json();
    value["unknown"] = true.into();
    assert!(SnapshotManifestV1::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value = manifest_json();
    value["domains"][0]["kind"] = "private-keys".into();
    assert!(SnapshotManifestV1::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value = manifest_json();
    let duplicate = value["domains"][0].clone();
    value["domains"].as_array_mut().unwrap().push(duplicate);
    value["file_count"] = 2.into();
    value["total_bytes"] = 8.into();
    assert!(SnapshotManifestV1::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value = manifest_json();
    let duplicate = value["domains"][0]["entries"][0].clone();
    value["domains"][0]["entries"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    value["file_count"] = 2.into();
    value["total_bytes"] = 8.into();
    assert!(SnapshotManifestV1::from_bytes(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn a_private_key_inside_a_native_domain_cannot_be_packaged() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("database");
    fs::create_dir(&database).unwrap();
    let key = database.join("validator.key");
    fs::write(&key, b"protected key sentinel").unwrap();
    let domains = [ResolvedDomain {
        name: "execution-db".to_owned(),
        root: database.clone(),
    }];
    let protected = ProtectedPaths(vec![key.clone()]);
    let output = temp.path().join("snapshot.tar");

    assert!(validate_layout(&domains, &protected, &[output]).is_err());
    assert_eq!(fs::read(&key).unwrap(), b"protected key sentinel");
    assert_eq!(fs::read_dir(database).unwrap().count(), 1);
}

#[test]
fn separate_native_roots_and_external_output_are_accepted_without_creation() {
    let temp = TempDir::new().unwrap();
    let mut domains = Vec::new();
    for name in ["execution-db", "ce", "offchain-primary"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        domains.push(ResolvedDomain {
            name: name.to_owned(),
            root,
        });
    }
    let protected = ProtectedPaths(vec![temp.path().join("keys/validator.key")]);
    let output: PathBuf = temp.path().join("output/snapshot.tar");
    validate_layout(&domains, &protected, std::slice::from_ref(&output)).unwrap();
    assert!(!output.parent().unwrap().exists());
    assert!(!temp.path().join("keys").exists());
}

#[test]
fn aliases_cannot_hide_native_overlap_or_an_output_inside_a_source() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("native");
    fs::create_dir(&root).unwrap();
    let alias = temp.path().join("alias");
    symlink(&root, &alias).unwrap();
    let domains = [
        ResolvedDomain {
            name: "execution-db".into(),
            root: root.clone(),
        },
        ResolvedDomain {
            name: "ce".into(),
            root: alias.clone(),
        },
    ];
    assert!(validate_layout(&domains, &ProtectedPaths::default(), &[]).is_err());
    assert!(validate_layout(
        &domains[..1],
        &ProtectedPaths::default(),
        &[alias.join("new/snapshot.tar")]
    )
    .is_err());
    assert!(validate_layout(
        &domains[..1],
        &ProtectedPaths(vec![alias.join("absent-key")]),
        &[]
    )
    .is_err());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn nested_domains_and_protected_ancestors_are_rejected() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("native");
    let child = root.join("child");
    fs::create_dir_all(&child).unwrap();
    let domains = [
        ResolvedDomain {
            name: "execution-db".into(),
            root,
        },
        ResolvedDomain {
            name: "ce".into(),
            root: child,
        },
    ];
    assert!(validate_layout(&domains, &ProtectedPaths::default(), &[]).is_err());
    assert!(validate_layout(
        &domains[1..],
        &ProtectedPaths(vec![temp.path().to_path_buf()]),
        &[]
    )
    .is_err());
    assert!(validate_layout(
        &domains[1..],
        &ProtectedPaths::default(),
        &[temp.path().to_path_buf()]
    )
    .is_err());
}

#[test]
fn output_cannot_overlap_protected_paths_or_another_output() {
    let temp = TempDir::new().unwrap();
    let secret = temp.path().join("identity/key");
    assert!(validate_layout(&[], &ProtectedPaths(vec![secret.clone()]), &[secret]).is_err());
    assert!(validate_layout(
        &[],
        &ProtectedPaths::default(),
        &[
            temp.path().join("output"),
            temp.path().join("output/scratch"),
        ]
    )
    .is_err());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn missing_sources_duplicate_labels_and_dangling_aliases_fail_without_writes() {
    let temp = TempDir::new().unwrap();
    let absent = temp.path().join("absent");
    let domain = ResolvedDomain {
        name: "execution-db".into(),
        root: absent.clone(),
    };
    assert!(validate_layout(&[domain], &ProtectedPaths::default(), &[]).is_err());
    assert!(!absent.exists());
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let domains = [
        ResolvedDomain {
            name: "execution-db".into(),
            root: first,
        },
        ResolvedDomain {
            name: "execution-db".into(),
            root: second,
        },
    ];
    assert!(validate_layout(&domains, &ProtectedPaths::default(), &[]).is_err());
    let dangling = temp.path().join("dangling");
    symlink(&absent, &dangling).unwrap();
    assert!(validate_layout(&[], &ProtectedPaths::default(), &[dangling]).is_err());
    assert!(!absent.exists());
}

#[test]
fn similarly_named_siblings_are_not_treated_as_nested_roots() {
    let temp = TempDir::new().unwrap();
    let mut domains = Vec::new();
    for name in ["db", "db-backup"] {
        let root = temp.path().join(name);
        fs::create_dir(&root).unwrap();
        domains.push(ResolvedDomain {
            name: name.into(),
            root,
        });
    }
    validate_layout(&domains, &ProtectedPaths::default(), &[]).unwrap();
}

#[test]
fn manifest_preserves_declared_paths_and_member_order() {
    use outbe_snapshot::manifest::SnapshotManifestV1;
    let mut value = manifest_json();
    value["domains"][0]["native_path"] = "/srv/node/db".into();
    value["domains"][0]["mode"] = 0o2750.into();
    value["domains"][0]["entries"][0]["mode"] = 0o4755.into();
    value["domains"][0]["entries"][0]["path"] = "/srv/node/db/z.dat".into();
    let mut second = value["domains"][0]["entries"][0].clone();
    second["path"] = "../other/a.dat".into();
    value["domains"][0]["entries"]
        .as_array_mut()
        .unwrap()
        .push(second);
    value["file_count"] = 2.into();
    value["total_bytes"] = 8.into();
    let mut another_domain = value["domains"][0].clone();
    another_domain["id"] = "aaa-other".into();
    value["domains"]
        .as_array_mut()
        .unwrap()
        .push(another_domain);
    value["file_count"] = 4.into();
    value["total_bytes"] = 16.into();
    let raw = serde_json::to_vec(&value).unwrap();
    let parsed = SnapshotManifestV1::from_bytes(&raw).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
}
