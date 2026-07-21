use std::{fs, process::Command};

use tempfile::TempDir;
use xtask::release::sgx::{
    build_bundle_manifest, canonical_json, compare_unsigned_trees, parse_sigstruct_view,
    verify_signed_bundle, BundleSpec, SourceIdentity,
};

const SIGSTRUCT: &str = "Attributes:\n\
    mr_signer: dee850fda5f2fe2b157dbea629d5182e9d3bfef43b0d00ec13e71d500656589f\n\
    mr_enclave: c6f76b702ccb4764f5583bd9ea13c9d2464a90f6513d4931d4674baad816eedf\n\
    isv_prod_id: 1\n\
    isv_svn: 1\n\
    debug_enclave: False\n";

fn repo_spec() -> BundleSpec {
    BundleSpec::read(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../release/testnet-sgx-bundle-v1.json"),
    )
    .expect("repository bundle spec")
}

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under repository root")
        .to_owned()
}

#[test]
fn repository_contract_has_no_runtime_signing_or_direct_fallback() {
    let root = repo_root();
    let entrypoint = fs::read_to_string(root.join("bin/outbe-tee-enclave/gramine/entrypoint.sh"))
        .expect("release entrypoint");
    assert!(!entrypoint.contains("gramine-sgx-sign"));
    assert!(!entrypoint.contains("gramine-sgx-gen-private-key"));
    assert!(!entrypoint.contains("gramine-direct"));
    assert!(!entrypoint.contains("enclave-key.pem"));
    assert!(entrypoint.contains("exec"));

    let dockerfile = fs::read_to_string(root.join("bin/outbe-tee-enclave/gramine/Dockerfile"))
        .expect("release Dockerfile");
    assert!(!dockerfile.contains("gramine-sgx-gen-private-key"));
    assert!(dockerfile.contains("@sha256:"));

    let test_dockerfile =
        fs::read_to_string(root.join("bin/outbe-tee-enclave/gramine/Dockerfile.test"))
            .expect("test Dockerfile");
    assert!(!test_dockerfile.contains("gramine-sgx-gen-private-key"));
    let test_entrypoint =
        fs::read_to_string(root.join("bin/outbe-tee-enclave/gramine/entrypoint.test.sh"))
            .expect("test entrypoint");
    assert!(test_entrypoint.contains("/run/secrets/outbe-test-sgx-key.pem"));
    assert!(test_entrypoint.contains("gramine-sgx-sign"));

    let template = fs::read_to_string(
        root.join("bin/outbe-tee-enclave/gramine/outbe-tee-enclave.release.manifest.template"),
    )
    .expect("release manifest template");
    assert!(template.contains("sgx.debug = false"));
    assert!(template.contains("sgx.remote_attestation = \"none\""));
    assert!(!template.contains("gramine-direct"));

    let adapter =
        fs::read_to_string(root.join("scripts/release/build-testnet-sgx-bundle-in-container.sh"))
            .expect("Gramine container adapter");
    assert!(adapter.contains("--chroot \"${bundle_root}\""));
}

#[test]
fn cli_exposes_typed_sgx_release_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["release", "sgx", "--help"])
        .output()
        .expect("run xtask help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help");
    for command in ["prepare", "compare", "sign", "verify"] {
        assert!(
            stdout.contains(command),
            "missing command {command}: {stdout}"
        );
    }
}

fn signed_fixture() -> TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    for relative in [
        "rootfs/opt/outbe/sgx/bin/outbe-tee-enclave",
        "rootfs/opt/outbe/sgx/gramine/libpal.so",
        "rootfs/opt/outbe/sgx/gramine/loader",
        "rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest",
        "rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest.sgx",
        "rootfs/opt/outbe/sgx/outbe-tee-enclave.sig",
    ] {
        let path = temp.path().join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("create fixture dirs");
        fs::write(path, relative.as_bytes()).expect("write fixture");
    }
    temp
}

#[test]
fn parses_sigstruct_into_typed_measurements() {
    let measurements = parse_sigstruct_view(SIGSTRUCT).expect("valid SIGSTRUCT");
    assert!(!measurements.debug);
    assert_eq!(measurements.isv_prod_id, 1);
    assert_eq!(measurements.isv_svn, 1);
    assert_eq!(
        measurements.mrenclave,
        "c6f76b702ccb4764f5583bd9ea13c9d2464a90f6513d4931d4674baad816eedf"
    );
}

#[test]
fn compares_independent_unsigned_trees_and_rejects_drift() {
    let first = tempfile::tempdir().expect("first");
    let second = tempfile::tempdir().expect("second");
    fs::create_dir(first.path().join("rootfs")).expect("first rootfs");
    fs::create_dir(second.path().join("rootfs")).expect("second rootfs");
    fs::write(first.path().join("rootfs/enclave"), b"same").expect("first artifact");
    fs::write(second.path().join("rootfs/enclave"), b"same").expect("second artifact");

    let evidence = compare_unsigned_trees(first.path(), second.path()).expect("identical");
    assert_eq!(evidence.result, "identical");
    assert_eq!(evidence.entry_count, 2);
    assert_eq!(evidence.tree_digest.algorithm, "sha256");

    fs::write(second.path().join("rootfs/enclave"), b"changed").expect("change artifact");
    let error = compare_unsigned_trees(first.path(), second.path()).expect_err("must differ");
    assert!(error.to_string().contains("unsigned SGX bundle mismatch"));
}

#[test]
fn canonical_manifest_binds_identity_measurements_and_every_bundle_file() {
    let fixture = signed_fixture();
    let manifest = build_bundle_manifest(
        fixture.path(),
        &repo_spec(),
        &SourceIdentity {
            source_commit: "a".repeat(40),
            source_date_epoch: 1_784_636_360,
            release_tag: "v0.1.1-testnet.1".to_owned(),
        },
        SIGSTRUCT,
    )
    .expect("build manifest");

    assert_eq!(manifest.authorization_scope, "testnet");
    assert_eq!(manifest.sigstruct_date, "2026-07-21");
    assert_eq!(manifest.files.len(), 6);
    let bytes = canonical_json(&manifest).expect("canonical JSON");
    assert_eq!(bytes.last(), Some(&b'\n'));
    verify_signed_bundle(fixture.path(), &manifest, &repo_spec(), SIGSTRUCT)
        .expect("valid signed fixture");
}

#[test]
fn verification_rejects_artifact_substitution() {
    let fixture = signed_fixture();
    let spec = repo_spec();
    let manifest = build_bundle_manifest(
        fixture.path(),
        &spec,
        &SourceIdentity {
            source_commit: "a".repeat(40),
            source_date_epoch: 1_784_636_360,
            release_tag: "v0.1.1-testnet.1".to_owned(),
        },
        SIGSTRUCT,
    )
    .expect("build manifest");
    fs::write(
        fixture
            .path()
            .join("rootfs/opt/outbe/sgx/outbe-tee-enclave.sig"),
        b"substituted",
    )
    .expect("substitute signature");

    let error = verify_signed_bundle(fixture.path(), &manifest, &spec, SIGSTRUCT)
        .expect_err("substitution must fail");
    assert!(error.to_string().contains("bundle file matrix mismatch"));
}
