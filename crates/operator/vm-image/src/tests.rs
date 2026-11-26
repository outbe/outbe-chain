use std::path::PathBuf;

use super::*;

fn repository_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .wrap_err("resolve repository root")?;
    ensure!(
        output.status.success(),
        "git rev-parse --show-toplevel failed"
    );
    let root = String::from_utf8(output.stdout)?;
    fs::canonicalize(root.trim()).wrap_err("canonicalize repository root")
}

#[test]
fn pinned_dstack_rev_patches_and_app_compose_match_repository() {
    let root = repository_root().unwrap();
    assert_eq!(DSTACK_COMMIT.len(), 40);
    let patches = verify_pinned_patches(&root).unwrap();
    assert_eq!(patches.len(), 4);
    let patch_0004 = fs::read_to_string(
        root.join("deploy/outbe-vm/patches/0004-repro-build-load-and-env.patch"),
    )
    .unwrap();
    assert!(patch_0004.contains("--entrypoint /usr/bin/nice"));
    assert!(patch_0004.contains("-n 19 /usr/bin/ionice -c3 /usr/local/bin/dstack-mkosi-entrypoint"));

    let compose: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("deploy/outbe-vm/app-compose.json")).unwrap())
            .unwrap();
    assert_eq!(compose["runner"], "bash");
    assert_eq!(compose["storage_fs"], "ext4");
    assert_eq!(
        compose["bash_script"],
        "systemctl start --no-block outbe-validator.target\n"
    );
}

#[test]
fn parse_flavors_validates_and_deduplicates() {
    assert_eq!(
        parse_flavors("dev,prod,dev").unwrap(),
        vec![Flavor::Dev, Flavor::Prod]
    );
    assert!(parse_flavors("").is_err());
    assert!(parse_flavors("staging").is_err());
}

#[test]
fn rejects_corrupted_software_bundle_and_unverified_runtime_libs() {
    let temp = tempfile::tempdir().unwrap();
    let bad_bundle = temp.path().join("bad.tar.zst");
    fs::write(&bad_bundle, b"not-a-valid-bundle").unwrap();
    let out = temp.path().join("extracted");
    fs::create_dir(&out).unwrap();
    assert!(verify_and_extract(&bad_bundle, &out).is_err());

    let bad_libs = temp.path().join("bad-libs");
    fs::create_dir(&bad_libs).unwrap();
    for lib in PINNED_RUNTIME_LIBS {
        fs::write(bad_libs.join(lib.name), b"tampered").unwrap();
    }
    let err = verify_pinned_runtime_libs(&bad_libs).unwrap_err();
    assert!(err.to_string().contains("digest mismatch"), "{err}");
}
