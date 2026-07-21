use std::{fs, process::Command};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use xtask::release::sgx::{
    build_bundle_manifest, build_release_manifest_candidate, canonical_json,
    compare_unsigned_trees, normalize_cosign_json_output, parse_oci_descriptor,
    parse_sigstruct_view, verify_signed_bundle, write_deterministic_bundle_archive, BundleSpec,
    SourceIdentity, VerifiedReleaseInputs,
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
    for command in [
        "prepare", "compare", "sign", "verify", "archive", "image", "manifest",
    ] {
        assert!(
            stdout.contains(command),
            "missing command {command}: {stdout}"
        );
    }
}

#[test]
fn privileged_release_workflow_pins_source_and_never_replaces_assets() {
    let root = repo_root();
    let workflow = fs::read_to_string(root.join(".github/workflows/testnet-release.yml"))
        .expect("testnet release workflow");
    assert_eq!(
        workflow.matches("ref: ${{ inputs.release_tag }}").count(),
        1,
        "only the initial unprivileged build may resolve the input tag"
    );
    assert!(
        workflow
            .matches("ref: ${{ needs.build-and-compare.outputs.verified_commit }}")
            .count()
            >= 4
    );
    assert!(!workflow.contains("--clobber"));
    assert!(!workflow.contains("gh release upload"));
    assert!(workflow.contains("release ${RELEASE_TAG} already exists"));
    assert!(workflow.contains(".verification.verified == true"));
    assert!(workflow.contains("verified_tag_object"));
    assert!(workflow.contains("[.tag, .object.sha] | @tsv"));
    assert!(workflow.contains("test \"${signed_tag_name}\" = \"${RELEASE_TAG}\""));
    assert!(!workflow.contains("git ls-remote --exit-code origin"));
    assert!(workflow.contains("--draft --prerelease"));
    assert!(workflow.contains("cmp --silent"));
    assert!(workflow.contains("gh release edit \"${RELEASE_TAG}\" --draft=false"));
    assert!(workflow.contains("cosign-image-verification.json"));
    assert!(workflow.contains("cosign-sbom-verification.json"));
    assert!(workflow.contains("cosign-provenance-verification.json"));
    assert!(!workflow.contains("buildkit-provenance.txt"));

    let cargo_config =
        fs::read_to_string(root.join(".cargo/config.toml")).expect("Cargo configuration");
    assert!(cargo_config.contains("xtask = \"run --locked --package xtask --\""));
}

#[test]
fn parses_buildkit_oci_descriptor_and_rejects_missing_digest() {
    let descriptor = parse_oci_descriptor(
        r#"{
          "containerimage.descriptor": {
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "size": 856
          },
          "containerimage.digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }"#,
    )
    .expect("valid BuildKit metadata");
    assert_eq!(descriptor.size, 856);
    assert_eq!(descriptor.digest.value.len(), 64);
    assert_eq!(
        descriptor.media_type,
        "application/vnd.oci.image.index.v1+json"
    );

    let error = parse_oci_descriptor(r#"{"containerimage.descriptor": {}}"#)
        .expect_err("missing digest must fail");
    assert!(error.to_string().contains("OCI descriptor digest"));
}

#[test]
fn normalizes_cosign_array_and_ndjson_output() {
    let normalized =
        normalize_cosign_json_output("[{\"critical\":1}]\n{\"payload\":\"abc\"}\n", "fixture")
            .expect("normalize Cosign output");
    assert_eq!(
        normalized,
        serde_json::json!([{"critical": 1}, {"payload": "abc"}])
    );
    assert!(normalize_cosign_json_output("", "fixture").is_err());
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

#[test]
fn release_manifest_candidate_binds_bundle_image_sbom_and_hardware_evidence() {
    let root = tempfile::tempdir().expect("tempdir");
    let fixture = signed_fixture();
    let source = SourceIdentity {
        source_commit: "a".repeat(40),
        source_date_epoch: 1_784_636_360,
        release_tag: "v0.1.1-testnet.1".to_owned(),
    };
    let bundle_manifest = build_bundle_manifest(fixture.path(), &repo_spec(), &source, SIGSTRUCT)
        .expect("bundle manifest");
    fs::create_dir_all(fixture.path().join("metadata")).expect("metadata");
    fs::write(
        fixture.path().join("metadata/testnet-sgx-bundle.json"),
        canonical_json(&bundle_manifest).expect("canonical bundle manifest"),
    )
    .expect("write bundle manifest");
    let bundle_manifest_digest = hex::encode(Sha256::digest(
        fs::read(fixture.path().join("metadata/testnet-sgx-bundle.json"))
            .expect("read bundle manifest"),
    ));

    let elf_manifest = root.path().join("release-manifest.json");
    let elf = serde_json::json!({
        "$schema": "https://outbe.io/schemas/release-manifest-v1.json",
        "artifacts": [{
            "classification": "production",
            "digest": {"algorithm": "sha256", "value": "a".repeat(64)},
            "features": [],
            "install_profiles": ["full-node", "validator"],
            "kind": "elf",
            "media_type": "application/vnd.outbe.elf",
            "name": "outbe-tee-enclave",
            "network_compatibility": "network-manifest-required",
            "package": "outbe-tee-enclave",
            "path": "bin/outbe-tee-enclave",
            "platform": {"architecture": "x86_64", "os": "linux", "target": "x86_64-unknown-linux-gnu"},
            "role": "tee-enclave",
            "size": 1,
            "tee": {"mock": false, "stage": "unsigned-bare-elf"}
        }],
        "build": {
            "provenance": {"entrypoint": "scripts/release/reproducible-build.sh", "mode": "local-container"},
            "source_date_epoch": source.source_date_epoch
        },
        "canonicalization": {},
        "inputs": [],
        "release": {
            "lifecycle": "build-candidate",
            "source": {"clean_tree_policy": "required", "commit": source.source_commit, "tree_state": "clean"},
            "tag": source.release_tag
        },
        "schema_version": "1.0.0",
        "verification_gates": []
    });
    fs::write(
        &elf_manifest,
        canonical_json(&elf).expect("canonical ELF manifest"),
    )
    .expect("write ELF manifest");

    let oci_evidence = root.path().join("oci.json");
    let oci = serde_json::json!({
        "bundle_manifest_digest": {"algorithm": "sha256", "value": bundle_manifest_digest},
        "image": {
            "digest": {"algorithm": "sha256", "value": "c".repeat(64)},
            "media_type": "application/vnd.oci.image.index.v1+json",
            "size": 856
        },
        "image_reference": "ghcr.io/outbe/outbe-tee-enclave-testnet:v0.1.1-testnet.1",
        "measurements": bundle_manifest.measurements,
        "platform": "linux/amd64",
        "provenance_attestation": true,
        "sbom_attestation": true,
        "schema_version": "1.0.0",
        "source": bundle_manifest.source
    });
    fs::write(
        &oci_evidence,
        canonical_json(&oci).expect("canonical OCI evidence"),
    )
    .expect("write OCI evidence");

    let bundle_archive = root.path().join("outbe-tee-enclave-sgx.tar");
    let cosign_image_verification = root.path().join("cosign-image-verification.json");
    let cosign_provenance_verification = root.path().join("cosign-provenance-verification.json");
    let cosign_sbom_verification = root.path().join("cosign-sbom-verification.json");
    let sbom = root.path().join("outbe-tee-enclave.spdx.json");
    let elf_evidence = root.path().join("elf-reproducibility.json");
    let sgx_evidence = root.path().join("sgx-reproducibility.json");
    let hardware_evidence = root.path().join("hardware-sgx.json");
    write_deterministic_bundle_archive(fixture.path(), &bundle_archive, source.source_date_epoch)
        .expect("archive bundle fixture");
    let sbom_value = serde_json::json!({"spdxVersion": "SPDX-2.3"});
    fs::write(&sbom, canonical_json(&sbom_value).expect("canonical SBOM")).expect("SBOM");
    let image_digest = format!("sha256:{}", "c".repeat(64));
    fs::write(
        &cosign_image_verification,
        canonical_json(&serde_json::json!([{
            "critical": {
                "identity": {"docker-reference": "ghcr.io/outbe/outbe-tee-enclave-testnet"},
                "image": {"docker-manifest-digest": image_digest},
                "type": "cosign container image signature"
            },
            "optional": null
        }]))
        .expect("canonical image verification"),
    )
    .expect("image verification");
    let attestation = |predicate_type: &str, predicate: serde_json::Value| {
        let statement = serde_json::json!({
            "_type": "https://in-toto.io/Statement/v0.1",
            "predicateType": predicate_type,
            "subject": [{"name": "ghcr.io/outbe/outbe-tee-enclave-testnet", "digest": {"sha256": "c".repeat(64)}}],
            "predicate": predicate
        });
        serde_json::json!([{
            "payload": BASE64.encode(canonical_json(&statement).expect("canonical statement")),
            "payloadType": "application/vnd.in-toto+json",
            "signatures": [{"sig": "verified-by-cosign"}]
        }])
    };
    fs::write(
        &cosign_sbom_verification,
        canonical_json(&attestation("https://spdx.dev/Document", sbom_value))
            .expect("canonical SBOM verification"),
    )
    .expect("SBOM verification");
    fs::write(
        &cosign_provenance_verification,
        canonical_json(&attestation(
            "https://slsa.dev/provenance/v0.2",
            serde_json::json!({
                "buildType": "https://mobyproject.org/buildkit@v1",
                "builder": {"id": ""},
                "materials": [{
                    "uri": "pkg:docker/gramineproject/gramine@1.8.1",
                    "digest": {"sha256": "d".repeat(64)}
                }]
            }),
        ))
        .expect("canonical provenance verification"),
    )
    .expect("provenance verification");
    fs::write(&elf_evidence, b"{\"result\":\"passed\"}\n").expect("ELF evidence");
    fs::write(&sgx_evidence, b"{\"result\":\"identical\"}\n").expect("SGX evidence");
    let hardware = serde_json::json!({
        "environment": {"backend": "gramine-sgx", "hardware_sgx": true},
        "image": {"digest": {"algorithm": "sha256", "value": "c".repeat(64)}},
        "measurements": bundle_manifest.measurements,
        "result": "passed"
    });
    fs::write(
        &hardware_evidence,
        canonical_json(&hardware).expect("canonical hardware evidence"),
    )
    .expect("hardware evidence");

    let inputs = VerifiedReleaseInputs {
        bundle: fixture.path().to_owned(),
        bundle_archive,
        cosign_image_verification,
        cosign_provenance_verification,
        cosign_sbom_verification,
        elf_evidence,
        elf_manifest,
        hardware_evidence,
        oci_evidence,
        sbom,
        sgx_evidence,
    };
    let manifest = build_release_manifest_candidate(&inputs).expect("release manifest candidate");

    assert_eq!(manifest["release"]["lifecycle"], "build-candidate");
    assert_eq!(manifest["build"]["provenance"]["mode"], "github-actions");
    assert_eq!(
        manifest["build"]["provenance"]["certificate_workflow_sha"],
        source.source_commit
    );
    assert_eq!(
        manifest["artifacts"].as_array().expect("artifacts").len(),
        4
    );
    let signed = &manifest["artifacts"][1];
    assert_eq!(signed["tee"]["stage"], "signed");
    assert_eq!(
        signed["tee"]["mrsigner"],
        bundle_manifest.measurements.mrsigner
    );
    assert_eq!(
        manifest["verification_gates"]
            .as_array()
            .expect("gates")
            .len(),
        6
    );
    assert!(manifest["verification_gates"]
        .as_array()
        .expect("gates")
        .iter()
        .all(|gate| gate["status"] == "passed"));
    let oci_gate = manifest["verification_gates"]
        .as_array()
        .expect("gates")
        .iter()
        .find(|gate| gate["name"] == "immutable-oci-sbom-and-provenance")
        .expect("OCI gate");
    assert_eq!(oci_gate["evidence"].as_array().expect("evidence").len(), 4);

    fs::write(
        &inputs.cosign_sbom_verification,
        canonical_json(&attestation(
            "https://spdx.dev/Document",
            serde_json::json!({"spdxVersion": "SPDX-2.2"}),
        ))
        .expect("mismatched verification"),
    )
    .expect("replace verification");
    let error = build_release_manifest_candidate(&inputs)
        .expect_err("attested SBOM substitution must fail");
    assert!(error.to_string().contains("attested SBOM"));
}
