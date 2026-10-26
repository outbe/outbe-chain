use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tempfile::tempdir;
use xtask::release::{sgx, toolchain};

fn repo_root() -> PathBuf {
    sgx::repository_root().expect("repository root")
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
}

#[test]
fn release_context_includes_compile_time_solidity_inputs() {
    let root = repo_root();
    let mut rs_files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut rs_files);
    let mut imported_domains = BTreeSet::new();
    for file in rs_files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for chunk in text.split("contracts/").skip(1) {
            if let Some((domain, _)) = chunk.split_once('/') {
                if !domain.is_empty()
                    && domain
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
                {
                    imported_domains.insert(domain.to_string());
                }
            }
        }
    }
    assert!(!imported_domains.is_empty());
    let dockerignore = fs::read_to_string(root.join(".dockerignore")).expect(".dockerignore");
    let rules: BTreeSet<&str> = dockerignore.lines().collect();
    for domain in imported_domains {
        assert!(rules.contains(format!("!contracts/{domain}").as_str()));
        assert!(rules.contains(format!("!contracts/{domain}/**").as_str()));
    }
}

#[test]
fn project_declares_one_version_pin_without_registry_delivery_and_consumers_follow_it() {
    let root = repo_root();
    let pin = toolchain::load_and_validate_pin(&root.join("release/project-toolchain-v1.json"))
        .expect("valid project toolchain pin");
    assert_eq!(pin["schema_version"].as_i64(), Some(1));
    assert_eq!(
        pin["activation"].as_str(),
        Some("pending-container-delivery")
    );
    assert_eq!(pin["target"].as_str(), Some("x86_64-unknown-linux-gnu"));
    assert_eq!(pin["platform"].as_str(), Some("linux/amd64"));
    assert_eq!(pin["rust"]["version"].as_str(), Some("1.96.0"));
    assert_eq!(pin["gramine"]["version"].as_str(), Some("1.9"));
    assert_eq!(
        pin["host_only_packages"]["libsgx-dcap-default-qpl"].as_str(),
        Some("1.26.100.1-noble1")
    );
    assert_eq!(
        pin["system_packages"]["libsgx-dcap-quote-verify"].as_str(),
        Some("1.26.100.1-noble1")
    );
    assert_eq!(
        pin["system_packages"]["libsgx-headers"].as_str(),
        Some("2.29.100.1-noble1")
    );
    let obj = pin.as_object().unwrap();
    for forbidden in ["image", "registry", "digest"] {
        assert!(!obj.contains_key(forbidden));
    }

    toolchain::verify_repository(&root).expect("repository consumers follow pin");
}

#[test]
fn consumer_version_drift_and_incomplete_package_pin_fail_closed() {
    let root = repo_root();
    let dir = tempdir().expect("tempdir");
    for relative in [
        "Dockerfile.project-toolchain",
        "crates/system/tee/build.rs",
        "rust-toolchain.toml",
        "release/apt/ubuntu.sources",
        "release/dcap-native-qvl-v1.json",
        "release/project-toolchain-v1.json",
        "release/reproducible-elf-build-v1.json",
        "release/mainnet-sgx-bundle-v1.json",
        "release/testnet-sgx-bundle-v1.json",
    ] {
        let dest = dir.path().join(relative);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(root.join(relative), dest).unwrap();
    }

    let sgx_path = dir.path().join("release/testnet-sgx-bundle-v1.json");
    let mut sgx_spec: Value = serde_json::from_str(&fs::read_to_string(&sgx_path).unwrap()).unwrap();
    sgx_spec["gramine"]["version"] = json!("1.8");
    fs::write(&sgx_path, serde_json::to_string(&sgx_spec).unwrap()).unwrap();

    let err = toolchain::verify_repository(dir.path()).unwrap_err();
    assert!(err.to_string().contains("SGX Gramine version"));

    let mut pin: Value = serde_json::from_str(
        &fs::read_to_string(root.join("release/project-toolchain-v1.json")).unwrap(),
    )
    .unwrap();
    pin["system_packages"]
        .as_object_mut()
        .unwrap()
        .remove("libsgx-headers");
    let broken_pin = dir.path().join("broken-pin.json");
    fs::write(&broken_pin, serde_json::to_string(&pin).unwrap()).unwrap();
    let pin_err = toolchain::load_and_validate_pin(&broken_pin).unwrap_err();
    assert!(pin_err.to_string().contains("system package set"));
}

#[test]
fn toolchain_dockerfile_and_apt_inputs_contract() {
    let root = repo_root();
    let pin = toolchain::load_and_validate_pin(&root.join("release/project-toolchain-v1.json"))
        .expect("pin");
    let recipe = fs::read_to_string(root.join("Dockerfile.project-toolchain")).unwrap();
    assert!(recipe.contains("/opt/outbe/toolchain/verify-toolchain-in-container.sh"));
    assert!(recipe.contains("/opt/outbe/toolchain/project-toolchain-v1.json"));
    assert!(recipe.contains("verify-apt-inputs"));
    assert!(recipe.contains("--no-download"));
    assert!(recipe.contains("ENTRYPOINT []"));

    let lowered = recipe.to_ascii_lowercase();
    assert!(!lowered.contains("apt-get upgrade"));
    assert!(!lowered.contains("apt upgrade"));
    assert!(!lowered.contains("qpl"));
    assert!(!lowered.contains("pccs"));

    let dir = tempdir().expect("tempdir");
    for source in pin["apt_provenance"]["source_files"].as_array().unwrap() {
        let rel = source["path"].as_str().unwrap().trim_start_matches('/');
        let path = dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, rel.as_bytes()).unwrap();
    }
    let mut custom_pin = pin.clone();
    for source in custom_pin["apt_provenance"]["source_files"]
        .as_array_mut()
        .unwrap()
    {
        let rel = source["path"].as_str().unwrap().trim_start_matches('/');
        let bytes = fs::read(dir.path().join(rel)).unwrap();
        source["sha256"] = json!(sha256_bytes(&bytes));
    }
    let deb_dir = dir.path().join("debs");
    fs::create_dir_all(&deb_dir).unwrap();
    fs::write(deb_dir.join("a_1_amd64.deb"), b"a").unwrap();
    fs::write(deb_dir.join("b_1_amd64.deb"), b"b").unwrap();
    let ledger = toolchain::apt_ledger(&deb_dir).unwrap();
    assert_eq!(ledger.count, 2);
    assert!(ledger.ledger.contains("a_1_amd64.deb"));
    custom_pin["apt_provenance"]["deb_closure"] = json!({
        "count": ledger.count,
        "format": "sha256sum-lines-sorted-by-filename-v1",
        "sha256": ledger.sha256,
    });
    let custom_pin_path = dir.path().join("project-toolchain-v1.json");
    fs::write(
        &custom_pin_path,
        serde_json::to_string(&custom_pin).unwrap(),
    )
    .unwrap();
    toolchain::verify_apt_inputs(&custom_pin_path, dir.path(), &deb_dir)
        .expect("valid apt inputs");

    fs::write(deb_dir.join("b_1_amd64.deb"), b"substituted").unwrap();
    let err = toolchain::verify_apt_inputs(&custom_pin_path, dir.path(), &deb_dir).unwrap_err();
    assert!(err.to_string().contains("closure digest mismatch"));
}

fn synthetic_qvl_fixture(root: &Path, pin: &Value) -> (Value, Vec<Value>, Vec<Value>) {
    let mut artifacts = Vec::new();
    for (idx, mut expected) in toolchain::expected_artifacts(pin).into_iter().enumerate() {
        let role = expected["role"].as_str().unwrap().to_string();
        let payload = format!("artifact-{role}").into_bytes();
        let rel = format!("native/{idx}");
        let target = root.join(&rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, &payload).unwrap();
        expected["path"] = json!(format!("/{rel}"));
        expected["size"] = json!(payload.len());
        expected["sha256"] = json!(sha256_bytes(&payload));
        artifacts.push(expected);
    }

    let header_payload = b"pinned-sgx-qve-header";
    let header_rel = "native/sgx_qve_header.h";
    let header_target = root.join(header_rel);
    fs::create_dir_all(header_target.parent().unwrap()).unwrap();
    fs::write(&header_target, header_payload).unwrap();

    let mut input = toolchain::expected_build_inputs(pin).into_iter().next().unwrap();
    input["path"] = json!(format!("/{header_rel}"));
    input["size"] = json!(header_payload.len());
    input["sha256"] = json!(sha256_bytes(header_payload));
    let build_inputs = vec![input];

    let manifest = json!({
        "schema_version": 1,
        "status": "active",
        "target": "x86_64-unknown-linux-gnu",
        "gramine_version": "1.9",
        "intel_dcap": toolchain::expected_dcap(pin),
        "build_inputs": build_inputs,
        "artifacts": artifacts,
    });
    (manifest, artifacts, build_inputs)
}

#[test]
fn native_qvl_manifest_verification_and_install_contract() {
    let root = repo_root();
    let repo_qvl = toolchain::load_qvl_manifest(&root.join("release/dcap-native-qvl-v1.json"))
        .expect("repo qvl manifest");
    assert_eq!(repo_qvl["status"].as_str(), Some("active"));

    let pin = toolchain::load_and_validate_pin(&root.join("release/project-toolchain-v1.json"))
        .expect("pin");
    let mut custom_pin = pin.clone();
    custom_pin["system_packages"]["libsgx-dcap-quote-verify"] = json!("test-qvl-version");
    custom_pin["system_packages"]["libstdc++6"] = json!("test-cxx-version");
    assert_eq!(
        toolchain::expected_dcap(&custom_pin)["runtime_package_version"].as_str(),
        Some("test-qvl-version")
    );
    assert_eq!(
        toolchain::expected_artifacts(&custom_pin)[1]["package_version"].as_str(),
        Some("test-cxx-version")
    );

    let dir = tempdir().expect("tempdir");
    let (manifest, artifacts, build_inputs) = synthetic_qvl_fixture(dir.path(), &pin);
    let expectations = toolchain::QvlExpectations {
        pin: &pin,
        artifacts: &artifacts,
        build_inputs: &build_inputs,
        check_packages: false,
    };

    toolchain::verify_qvl_manifest_with_expectations(&manifest, dir.path(), &expectations)
        .expect("valid synthetic qvl manifest");

    let install_dir = dir.path().join("bundle-qvl");
    toolchain::install_verified_artifacts_with_expectations(
        &manifest,
        dir.path(),
        &install_dir,
        &expectations,
    )
    .expect("install verified artifacts");

    for artifact in &artifacts {
        let install_name = artifact["install_name"].as_str().unwrap();
        let role = artifact["role"].as_str().unwrap();
        let installed = install_dir.join(install_name);
        assert_eq!(
            fs::read(&installed).unwrap(),
            format!("artifact-{role}").as_bytes()
        );
        assert_eq!(
            fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    let nonempty_err = toolchain::install_verified_artifacts_with_expectations(
        &manifest,
        dir.path(),
        &install_dir,
        &expectations,
    )
    .unwrap_err();
    assert!(nonempty_err.to_string().contains("destination must be empty"));
}

#[test]
fn native_qvl_verifier_rejects_tampered_inputs_and_boundaries() {
    let root = repo_root();
    let pin = toolchain::load_and_validate_pin(&root.join("release/project-toolchain-v1.json"))
        .expect("pin");
    let dir = tempdir().expect("tempdir");
    let (manifest, artifacts, build_inputs) = synthetic_qvl_fixture(dir.path(), &pin);
    let expectations = toolchain::QvlExpectations {
        pin: &pin,
        artifacts: &artifacts,
        build_inputs: &build_inputs,
        check_packages: false,
    };

    let mut bad_status = manifest.clone();
    bad_status["status"] = json!("inactive-until-i9");
    let status_err =
        toolchain::verify_qvl_manifest_with_expectations(&bad_status, dir.path(), &expectations)
            .unwrap_err();
    assert!(status_err.to_string().contains("status"));

    let mut bad_qve = manifest.clone();
    bad_qve["intel_dcap"]["qve_enabled"] = json!(true);
    let qve_err =
        toolchain::verify_qvl_manifest_with_expectations(&bad_qve, dir.path(), &expectations)
            .unwrap_err();
    assert!(qve_err.to_string().contains("Intel DCAP metadata"));

    fs::write(dir.path().join("native/0"), b"substituted").unwrap();
    let size_err =
        toolchain::verify_qvl_manifest_with_expectations(&manifest, dir.path(), &expectations)
            .unwrap_err();
    assert!(size_err.to_string().contains("size mismatch"));
}
