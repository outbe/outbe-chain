use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use xtask::release::elf::{
    artifact_build_plan, artifact_check_plan, build_manifest, load_and_validate_build_spec,
    resolve_release_identity, validate_release_manifest_v1, verify_outputs, verify_version_text,
    ReleaseManifestOptions, EXPECTED_ARTIFACTS, EXPECTED_CHECKSUM_PATHS,
};
use xtask::release::sgx::canonical_json;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under repository root")
        .to_owned()
}

fn init_git_repo() -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .arg(root)
        .status()
        .unwrap()
        .success());
    for (k, v) in [
        ("user.email", "release@example.test"),
        ("user.name", "Release Test"),
    ] {
        assert!(Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", k, v])
            .status()
            .unwrap()
            .success());
    }
    fs::write(root.join("source"), b"first\n").unwrap();
    assert!(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "source"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-q", "-m", "first"])
        .status()
        .unwrap()
        .success());
    let first = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    (dir, first)
}

#[cfg(unix)]
fn export_repo_without_git(destination: &Path) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(["archive", "--format=tar", "HEAD"])
        .output()
        .expect("export repository archive");
    assert!(output.status.success(), "git archive failed");
    tar::Archive::new(output.stdout.as_slice())
        .unpack(destination)
        .expect("unpack repository archive");
    assert!(!destination.join(".git").exists());
}

#[cfg(unix)]
fn assert_command_succeeded(output: std::process::Output, command: &str) {
    assert!(
        output.status.success(),
        "{command} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[cfg(unix)]
#[test]
fn explicit_elf_commands_run_without_git_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_root = tmp.path().join("workspace");
    fs::create_dir(&source_root).expect("create workspace");
    export_repo_without_git(&source_root);

    let fake_bin = tmp.path().join("fake-bin");
    fs::create_dir(&fake_bin).expect("create fake bin directory");
    let fake_cargo = fake_bin.join("cargo");
    fs::write(&fake_cargo, "#!/bin/sh\nexit 0\n").expect("write fake cargo");
    let mut permissions = fs::metadata(&fake_cargo).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_cargo, permissions).expect("make fake cargo executable");
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(fake_bin).chain(std::env::split_paths(&current_path)))
            .expect("compose PATH");

    let build_spec = source_root.join("release/reproducible-elf-build-v1.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&source_root)
        .env("PATH", &path)
        .args(["release", "elf", "inputs", "--build-spec"])
        .arg(&build_spec)
        .arg("--repo-root")
        .arg(&source_root)
        .arg("--execute-artifact-build-plan")
        .output()
        .expect("run release elf inputs");
    assert_command_succeeded(output, "release elf inputs");

    let source_commit = "a".repeat(40);
    let source_date_epoch = 1_784_000_000_i64;
    let source_date_epoch_arg = source_date_epoch.to_string();
    let timestamp = xtask::release::elf::expected_timestamp(source_date_epoch).unwrap();
    let version_file = tmp.path().join("outbe-chain.version.txt");
    fs::write(
        &version_file,
        format!(
            "Commit SHA: {source_commit}\nBuild Timestamp: {timestamp}\n\
             Build Features: no features enabled\n\
             Build Profile: release (x86_64-unknown-linux-gnu)\n\
             Commit SHA: {source_commit}\nBuild Timestamp: {timestamp}\n"
        ),
    )
    .expect("write version fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&source_root)
        .args(["release", "elf", "verify-version", "--version-file"])
        .arg(&version_file)
        .args(["--source-commit", &source_commit])
        .args(["--source-date-epoch", &source_date_epoch_arg])
        .args(["--target", "x86_64-unknown-linux-gnu"])
        .args(["--profile", "release"])
        .output()
        .expect("run release elf verify-version");
    assert_command_succeeded(output, "release elf verify-version");

    let artifact_dir = tmp.path().join("artifacts");
    fs::create_dir(&artifact_dir).expect("create artifact directory");
    for &name in EXPECTED_ARTIFACTS {
        fs::write(artifact_dir.join(name), b"\x7fELF deterministic fixture")
            .expect("write artifact fixture");
    }
    let packages = tmp.path().join("builder-system-packages.txt");
    fs::write(&packages, b"example=1.0\n").expect("write package inventory");
    let manifest = tmp.path().join("release-manifest.json");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&source_root)
        .args(["release", "elf", "generate-manifest", "--build-spec"])
        .arg(&build_spec)
        .arg("--source-root")
        .arg(&source_root)
        .arg("--artifact-dir")
        .arg(&artifact_dir)
        .args(["--release-tag", "test-no-git"])
        .args(["--source-commit", &source_commit])
        .args(["--source-date-epoch", &source_date_epoch_arg])
        .args(["--lifecycle", "build-candidate"])
        .arg("--verification-gates")
        .arg(source_root.join("release/reproducible-elf-candidate-gates-v1.json"))
        .arg("--resolved-system-packages")
        .arg(&packages)
        .arg("--output")
        .arg(&manifest)
        .output()
        .expect("run release elf generate-manifest");
    assert_command_succeeded(output, "release elf generate-manifest");
    assert!(manifest.is_file());
}

#[test]
fn ambient_tags_do_not_change_commit_identity_and_explicit_tags_resolve_to_head() {
    let (dir, first) = init_git_repo();
    let root = dir.path();

    let before = resolve_release_identity(root, None).unwrap();
    assert!(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["tag", "v99.0.0"])
        .status()
        .unwrap()
        .success());
    let after = resolve_release_identity(root, None).unwrap();
    assert_eq!(before, after);
    assert_eq!(after.release_tag, format!("commit-{first}"));
    assert_eq!(after.source_describe, after.release_tag);

    assert!(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["tag", "v1.0.0"])
        .status()
        .unwrap()
        .success());
    let tagged = resolve_release_identity(root, Some("v1.0.0")).unwrap();
    assert_eq!(tagged.release_tag, "v1.0.0");
    assert_eq!(tagged.source_describe, "v1.0.0");

    fs::write(root.join("source"), b"second\n").unwrap();
    assert!(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-qam", "second"])
        .status()
        .unwrap()
        .success());
    let err = resolve_release_identity(root, Some("v1.0.0")).unwrap_err();
    assert!(err.to_string().contains("not HEAD"));
}

#[test]
fn build_spec_validation_and_cargo_plans_enforce_production_matrix() {
    let root = repo_root();
    let spec_path = root.join("release/reproducible-elf-build-v1.json");
    let loaded = load_and_validate_build_spec(&spec_path, Some(&root)).unwrap();
    assert_eq!(
        loaded["project_toolchain"],
        "release/project-toolchain-v1.json"
    );

    let build_plan = artifact_build_plan(&loaded).unwrap();
    let check_plan = artifact_check_plan(&loaded).unwrap();
    assert_eq!(build_plan.len(), 2);
    assert_eq!(check_plan.len(), 2);
    assert!(build_plan[1]
        .windows(2)
        .any(|w| w == ["--features", "production-dcap-release"]));
    for (b, c) in build_plan.iter().zip(check_plan.iter()) {
        assert_eq!(&c[..2], &["cargo", "check"]);
        assert_eq!(&c[2..], &b[2..]);
    }

    let tmp = tempfile::tempdir().unwrap();
    let mut bad_builder = loaded.clone();
    bad_builder["builder"]["base_images"][0] = json!("rust:latest");
    let bad_path = tmp.path().join("bad-builder.json");
    fs::write(&bad_path, serde_json::to_vec(&bad_builder).unwrap()).unwrap();
    let err = load_and_validate_build_spec(&bad_path, None).unwrap_err();
    assert!(err.to_string().contains("sha256-pinned"));
}

#[test]
fn outbe_chain_version_verification_checks_commit_timestamp_profile_and_features() {
    let commit = "a".repeat(40);
    let ts = "2026-07-15T03:33:20.000000000Z";
    let valid = format!(
        "Commit SHA: {commit}\nBuild Timestamp: {ts}\n\
         Build Features: no features enabled\n\
         Build Profile: release (x86_64-unknown-linux-gnu)\n\
         Commit SHA: {commit}\nBuild Timestamp: {ts}\n"
    );
    assert!(verify_version_text(
        &valid,
        &commit,
        1_784_086_400,
        "x86_64-unknown-linux-gnu",
        "release"
    )
    .is_empty());

    let drift = verify_version_text(
        "Commit SHA: bad\nBuild Timestamp: now\nBuild Profile: debug\n",
        &commit,
        1_784_086_400,
        "x86_64-unknown-linux-gnu",
        "release",
    );
    assert!(drift.len() >= 3);
}

#[test]
fn release_manifest_is_canonical_and_enforces_schema_invariants() {
    let tmp = tempfile::tempdir().unwrap();
    let artifact_dir = tmp.path().join("artifacts");
    let input_dir = tmp.path().join("source");
    fs::create_dir_all(&artifact_dir).unwrap();
    fs::create_dir_all(&input_dir).unwrap();

    fs::write(input_dir.join("Cargo.lock"), b"locked\n").unwrap();
    fs::write(input_dir.join("rust-toolchain.toml"), b"1.96.0\n").unwrap();
    fs::write(artifact_dir.join("outbe-chain"), b"chain-elf").unwrap();
    fs::write(artifact_dir.join("outbe-tee-enclave"), b"enclave-elf").unwrap();

    let spec = json!({
        "spec_version": 1,
        "target": "x86_64-unknown-linux-gnu",
        "profile": "release",
        "rust_toolchain": "1.96.0",
        "builder": {
            "id": "https://github.com/outbe/outbe-chain/reproducible-elf-builder/v1",
            "base_images": [
                format!("rust:1.96.0-bookworm@sha256:{}", "1".repeat(64)),
                format!("gramineproject/gramine:1.9-noble@sha256:{}", "2".repeat(64)),
            ],
            "recipe": "Dockerfile.project-toolchain",
            "system_packages": ["clang=1.0", "cmake=1.0"],
        },
        "environment": {
            "cflags": "-ffile-prefix-map=/workspace=/usr/src/outbe-chain",
            "cxxflags": "-ffile-prefix-map=/workspace=/usr/src/outbe-chain",
            "locale": "C",
            "timezone": "UTC",
            "rustflags": ["--remap-path-prefix=/workspace=.", "-C", "link-arg=-Wl,--build-id=sha1"],
            "zero_ar_date": "1",
        },
        "cargo": {"auditable": false, "locked": true},
        "inputs": ["Cargo.lock", "rust-toolchain.toml"],
        "artifacts": [
            {
                "name": "outbe-chain",
                "package": "outbe-chain",
                "role": "node",
                "classification": "production",
                "features": [],
                "install_profiles": ["full-node", "validator"],
            },
            {
                "name": "outbe-tee-enclave",
                "package": "outbe-tee-enclave",
                "role": "tee-enclave",
                "classification": "production",
                "features": ["production-dcap-release"],
                "install_profiles": ["full-node", "validator"],
            },
        ],
    });

    let opts = ReleaseManifestOptions {
        release_tag: "v0.1.0-test".to_owned(),
        source_commit: "a".repeat(40),
        source_date_epoch: 1_784_000_000,
        lifecycle: "build-candidate".to_owned(),
        verification_gates: Vec::new(),
        resolved_system_packages: None,
    };

    let manifest = build_manifest(&spec, &input_dir, &artifact_dir, &opts).unwrap();
    validate_release_manifest_v1(&manifest).unwrap();
    let encoded = canonical_json(&manifest).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&encoded)),
        "c376695aaa8e6c60b890cb85aac304246455faba8389f8660b26b32814f07c4b"
    );

    let mut bad_verified = manifest.clone();
    bad_verified["release"]["lifecycle"] = json!("verified");
    assert!(validate_release_manifest_v1(&bad_verified).is_err());
}

fn write_checksums(output: &Path) {
    let mut rows = String::new();
    for &rel in EXPECTED_CHECKSUM_PATHS {
        let digest = hex::encode(Sha256::digest(fs::read(output.join(rel)).unwrap()));
        rows.push_str(&format!("{digest}  {rel}\n"));
    }
    fs::write(output.join("SHA256SUMS"), rows.as_bytes()).unwrap();
}

#[test]
fn verify_outputs_passes_on_identical_builders_and_detects_tampering() {
    let root = repo_root();
    let tmp = tempfile::tempdir().unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    fs::create_dir_all(first.join("bin")).unwrap();
    fs::create_dir_all(first.join("metadata")).unwrap();

    let spec: Value = serde_json::from_slice(
        &fs::read(root.join("release/reproducible-elf-build-v1.json")).unwrap(),
    )
    .unwrap();
    for (idx, art) in spec["artifacts"].as_array().unwrap().iter().enumerate() {
        let name = art["name"].as_str().unwrap();
        let mut bytes = b"\x7fELF".to_vec();
        bytes.push(idx as u8);
        bytes.extend_from_slice(b"deterministic-test");
        fs::write(first.join("bin").join(name), bytes).unwrap();
    }
    let packages = first.join("metadata/builder-system-packages.txt");
    fs::write(&packages, b"example=1.0\n").unwrap();
    let commit = "a".repeat(40);
    let version_text = format!(
        "Outbe 0.1.0 (test)\nVersion: 0.1.0\nCommit SHA: {commit}\n\
         Build Timestamp: 2026-07-14T03:33:20.000000000Z\n\
         Build Features: no features enabled\n\
         Build Profile: release (x86_64-unknown-linux-gnu)\n\n\
         Reth Version: test\nCommit SHA: {commit}\n\
         Build Timestamp: 2026-07-14T03:33:20.000000000Z\n\
         Build Features:\nBuild Profile: release\n"
    );
    fs::write(
        first.join("metadata/outbe-chain.version.txt"),
        version_text.as_bytes(),
    )
    .unwrap();
    fs::copy(
        root.join("release/release-manifest-v1.schema.json"),
        first.join("metadata/release-manifest-v1.schema.json"),
    )
    .unwrap();
    fs::copy(
        root.join("release/reproducible-elf-build-v1.json"),
        first.join("metadata/reproducible-elf-build-v1.json"),
    )
    .unwrap();

    let manifest = build_manifest(
        &spec,
        &root,
        &first.join("bin"),
        &ReleaseManifestOptions {
            release_tag: "test-rebuild".to_owned(),
            source_commit: commit,
            source_date_epoch: 1_784_000_000,
            lifecycle: "build-candidate".to_owned(),
            verification_gates: Vec::new(),
            resolved_system_packages: Some(packages),
        },
    )
    .unwrap();
    fs::write(
        first.join("release-manifest.json"),
        canonical_json(&manifest).unwrap(),
    )
    .unwrap();
    write_checksums(&first);

    fs::create_dir_all(second.join("bin")).unwrap();
    fs::create_dir_all(second.join("metadata")).unwrap();
    for &rel in EXPECTED_CHECKSUM_PATHS {
        fs::copy(first.join(rel), second.join(rel)).unwrap();
    }
    fs::copy(first.join("SHA256SUMS"), second.join("SHA256SUMS")).unwrap();

    let evidence = verify_outputs(&first, &second, &root, false).unwrap();
    assert_eq!(evidence["result"], "passed");
    assert_eq!(evidence["differences"], json!([]));

    let mut tampered = fs::read(second.join("bin/outbe-cli")).unwrap();
    tampered.extend_from_slice(b"changed");
    fs::write(second.join("bin/outbe-cli"), tampered).unwrap();
    let failed = verify_outputs(&first, &second, &root, false).unwrap();
    assert_eq!(failed["result"], "failed");
    assert!(failed["differences"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d.as_str().unwrap().contains("outbe-cli")));
}
