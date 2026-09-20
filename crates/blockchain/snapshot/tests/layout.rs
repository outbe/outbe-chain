use std::os::unix::fs::symlink;
use std::{fs, path::PathBuf};

use outbe_snapshot::layout::{validate_layout, ProtectedPaths, ResolvedDomain};
use tempfile::TempDir;

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
    let domains = [
        ResolvedDomain {
            name: "execution-db".into(),
            root: temp.path().to_path_buf(),
        },
        ResolvedDomain {
            name: "execution-db".into(),
            root: temp.path().to_path_buf(),
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
