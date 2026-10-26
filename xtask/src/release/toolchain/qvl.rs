//! Fail-closed verification and installation of the V1 native-QVL artifact contract.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{ensure, eyre, Result, WrapErr as _};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

const EXPECTED_ROLES: &[&str] = &["qvl", "cxx-runtime", "gcc-runtime"];
pub const EXPECTED_STATUS: &str = "active";

struct BaseArtifact {
    role: &'static str,
    package: &'static str,
    path: &'static str,
    install_name: &'static str,
    size: u64,
    sha256: &'static str,
    elf_build_id: Option<&'static str>,
}

struct BaseInput {
    role: &'static str,
    package: &'static str,
    path: &'static str,
    size: u64,
    sha256: &'static str,
}

const ARTIFACT_BASE: &[BaseArtifact] = &[
    BaseArtifact {
        role: "qvl",
        package: "libsgx-dcap-quote-verify",
        path: "/usr/lib/x86_64-linux-gnu/libsgx_dcap_quoteverify.so.1.13.103.0",
        install_name: "libsgx_dcap_quoteverify.so.1",
        size: 5_322_424,
        sha256: "4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef",
        elf_build_id: Some("663c0acf2b4673c22c66f01112c5f38d856fd5a5"),
    },
    BaseArtifact {
        role: "cxx-runtime",
        package: "libstdc++6",
        path: "/usr/lib/x86_64-linux-gnu/libstdc++.so.6.0.33",
        install_name: "libstdc++.so.6",
        size: 2_592_224,
        sha256: "1fd75fe70354a416d75aef22bcae68c47bd25d20e2d0568c30b1a9838cf62f11",
        elf_build_id: None,
    },
    BaseArtifact {
        role: "gcc-runtime",
        package: "libgcc-s1",
        path: "/usr/lib/x86_64-linux-gnu/libgcc_s.so.1",
        install_name: "libgcc_s.so.1",
        size: 183_024,
        sha256: "d93224d2b0dab4247598be683adca02f5cf00586f99c187579cd7e92058fb7cb",
        elf_build_id: None,
    },
];

const BUILD_INPUT_BASE: &[BaseInput] = &[
    BaseInput {
        role: "qvl-api-header",
        package: "libsgx-dcap-quote-verify-dev",
        path: "/usr/include/sgx_dcap_quoteverify.h",
        size: 18_643,
        sha256: "22311e0b4332276c315fdc2f792b28197a3d9a7bfd376246fa5fa3c1bef870ef",
    },
    BaseInput {
        role: "qve-status-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_qve_header.h",
        size: 10_590,
        sha256: "f8994fcb1b56ed938adbf923146b5fed8c3e8d5d7d6827f45342db0a23e56677",
    },
    BaseInput {
        role: "sgx-key-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_key.h",
        size: 4_211,
        sha256: "8cf1a8ef06f5ede977b15392e23a368b4a78e895fabf9680018d81a8e014c21d",
    },
    BaseInput {
        role: "sgx-attributes-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_attributes.h",
        size: 3_773,
        sha256: "ba8e81f44a76518920ab086546970d636c2379ca372e0dfb0fef4b3ce78ed8b5",
    },
    BaseInput {
        role: "qvl-quote-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_ql_quote.h",
        size: 4_630,
        sha256: "d88804c288c15f03958ac6b54d56513ce790ffc190fe32563a178ba5360fc3df",
    },
    BaseInput {
        role: "qvl-common-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_ql_lib_common.h",
        size: 20_764,
        sha256: "2bc3c7ef75c7230bc4b4a1b484800199e791fe78aee7a8161e78e6ebb6fc3c2d",
    },
    BaseInput {
        role: "sgx-quote-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_quote.h",
        size: 6_247,
        sha256: "1c4582245fffb8b8ca409af2de5810f83ad15f79c17183634757bb913cb51d68",
    },
    BaseInput {
        role: "sgx-report-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_report.h",
        size: 5_273,
        sha256: "2759b2579a5c62e8889b4296c3f9f6ff7fde2983823c67b825105585b796101b",
    },
    BaseInput {
        role: "sgx-defs-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_defs.h",
        size: 2_378,
        sha256: "a2be53305ec098f18e9e62550781e7e405c6b4485f87e31d6446bec5a6eefa0b",
    },
    BaseInput {
        role: "sgx-quote3-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_quote_3.h",
        size: 9_724,
        sha256: "d569890a4c1236e94c13a8673d4006e1542e04a313c26f8cdc2fdc8456a9d6ea",
    },
    BaseInput {
        role: "sgx-pce-header",
        package: "libsgx-headers",
        path: "/usr/include/sgx_pce.h",
        size: 5_748,
        sha256: "ecc31a29dc046bcb6bfea6bc6427600e0d98b43310c6edcc4f9e82284dc70968",
    },
];

pub fn expected_dcap(pin: &Value) -> Value {
    let pkgs = &pin["system_packages"];
    json!({
        "runtime_package": "libsgx-dcap-quote-verify",
        "runtime_package_version": pkgs["libsgx-dcap-quote-verify"],
        "development_package": "libsgx-dcap-quote-verify-dev",
        "development_package_version": pkgs["libsgx-dcap-quote-verify-dev"],
        "headers_package": "libsgx-headers",
        "headers_package_version": pkgs["libsgx-headers"],
        "collateral_major_version": 3,
        "collateral_minor_version": 1,
        "qve_report_info": "null",
        "qve_enabled": false,
        "tvl_enabled": false,
        "qpl_or_pccs_during_consensus": false,
    })
}

pub fn expected_artifacts(pin: &Value) -> Vec<Value> {
    let pkgs = &pin["system_packages"];
    ARTIFACT_BASE
        .iter()
        .map(|a| {
            let mut obj = json!({
                "role": a.role,
                "package": a.package,
                "package_version": pkgs[a.package],
                "path": a.path,
                "install_name": a.install_name,
                "size": a.size,
                "sha256": a.sha256,
            });
            if let Some(id) = a.elf_build_id {
                obj["elf_build_id"] = json!(id);
            }
            obj
        })
        .collect()
}

pub fn expected_build_inputs(pin: &Value) -> Vec<Value> {
    let pkgs = &pin["system_packages"];
    BUILD_INPUT_BASE
        .iter()
        .map(|b| {
            json!({
                "role": b.role,
                "package": b.package,
                "package_version": pkgs[b.package],
                "path": b.path,
                "size": b.size,
                "sha256": b.sha256,
            })
        })
        .collect()
}

pub fn load_qvl_manifest(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let val: Value = serde_json::from_str(&raw)?;
    ensure!(val.is_object(), "native-QVL manifest must be a JSON object");
    Ok(val)
}

fn artifact_path(root: &Path, configured: &str) -> Result<PathBuf> {
    let path = Path::new(configured);
    ensure!(
        path.is_absolute(),
        "native-QVL artifact path must be absolute: {configured}"
    );
    let root_canon = fs::canonicalize(root)?;
    let joined = root_canon.join(configured.trim_start_matches('/'));
    let candidate =
        fs::canonicalize(&joined).map_err(|_| eyre!("missing native-QVL file: {configured}"))?;
    ensure!(
        candidate.starts_with(&root_canon),
        "native-QVL artifact escapes root: {configured}"
    );
    Ok(candidate)
}

fn verify_installed_packages(dcap: &Value, artifacts: &[Value], inputs: &[Value]) -> Result<()> {
    let mut pkgs = BTreeMap::new();
    for (k, v) in [
        ("runtime_package", "runtime_package_version"),
        ("development_package", "development_package_version"),
        ("headers_package", "headers_package_version"),
    ] {
        if let (Some(p), Some(ver)) = (dcap[k].as_str(), dcap[v].as_str()) {
            pkgs.insert(p.to_owned(), ver.to_owned());
        }
    }
    for entry in artifacts.iter().chain(inputs.iter()) {
        if let (Some(p), Some(ver)) = (entry["package"].as_str(), entry["package_version"].as_str())
        {
            pkgs.insert(p.to_owned(), ver.to_owned());
        }
    }
    for (pkg, expected) in pkgs {
        let out = Command::new("dpkg-query")
            .args(["--show", "--showformat=${Version}", &pkg])
            .output()?;
        ensure!(
            out.status.success(),
            "required native-QVL package is not installed: {pkg}"
        );
        let actual = String::from_utf8_lossy(&out.stdout);
        ensure!(
            actual == expected,
            "native-QVL package version mismatch: {pkg} (expected {expected})"
        );
    }
    Ok(())
}

fn verify_entries(manifest: &Value, root: &Path, field: &str, expected: &[Value]) -> Result<()> {
    let kind = if field == "build_inputs" {
        "build input"
    } else {
        "artifact"
    };
    let entries = manifest
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("native-QVL manifest {field} must be an array"))?;
    ensure!(
        entries == expected,
        "native-QVL {kind} metadata does not match the exact V1 contract"
    );
    let roles: Vec<&str> = entries.iter().filter_map(|e| e["role"].as_str()).collect();
    let expected_roles: Vec<&str> = if field == "build_inputs" {
        expected.iter().filter_map(|e| e["role"].as_str()).collect()
    } else {
        EXPECTED_ROLES.to_vec()
    };
    ensure!(
        roles == expected_roles,
        "native-QVL {kind} roles must be {expected_roles:?}"
    );

    let mut seen_paths = BTreeSet::new();
    let mut seen_names = BTreeSet::new();
    for entry in entries {
        let configured = entry["path"]
            .as_str()
            .ok_or_else(|| eyre!("native-QVL {kind} path is missing"))?;
        ensure!(
            seen_paths.insert(configured),
            "native-QVL {kind} path is missing or duplicated"
        );
        if kind == "artifact" {
            let install_name = entry["install_name"]
                .as_str()
                .ok_or_else(|| eyre!("native-QVL install name is missing"))?;
            ensure!(
                seen_names.insert(install_name),
                "native-QVL install name is missing or duplicated"
            );
        }
        let expected_size = entry["size"]
            .as_u64()
            .filter(|&s| s > 0)
            .ok_or_else(|| eyre!("invalid native-QVL {kind} size for {configured}"))?;
        let expected_sha = entry["sha256"]
            .as_str()
            .filter(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            .ok_or_else(|| eyre!("invalid native-QVL {kind} SHA-256 for {configured}"))?;
        let path = artifact_path(root, configured)?;
        let payload = fs::read(&path)?;
        ensure!(
            payload.len() as u64 == expected_size,
            "native-QVL {kind} size mismatch: {configured}"
        );
        ensure!(
            hex::encode(Sha256::digest(&payload)) == expected_sha,
            "native-QVL {kind} SHA-256 mismatch: {configured}"
        );
    }
    Ok(())
}

pub struct QvlExpectations<'a> {
    pub pin: &'a Value,
    pub artifacts: &'a [Value],
    pub build_inputs: &'a [Value],
    pub check_packages: bool,
}

pub fn verify_qvl_manifest_with_expectations(
    manifest: &Value,
    root: &Path,
    expectations: &QvlExpectations<'_>,
) -> Result<()> {
    ensure!(
        manifest.get("schema_version").and_then(Value::as_i64) == Some(1),
        "unsupported native-QVL manifest schema"
    );
    ensure!(
        manifest.get("status").and_then(Value::as_str) == Some(EXPECTED_STATUS),
        "native-QVL status must be {EXPECTED_STATUS}"
    );
    ensure!(
        manifest.get("target").and_then(Value::as_str) == Some("x86_64-unknown-linux-gnu"),
        "native-QVL V1 target must be x86_64-unknown-linux-gnu"
    );
    let gramine_ver = expectations.pin["gramine"]["version"]
        .as_str()
        .unwrap_or("1.9");
    ensure!(
        manifest.get("gramine_version").and_then(Value::as_str) == Some(gramine_ver),
        "native-QVL V1 requires exact Gramine {gramine_ver}"
    );
    let dcap = manifest
        .get("intel_dcap")
        .ok_or_else(|| eyre!("native-QVL manifest is missing intel_dcap"))?;
    ensure!(
        *dcap == expected_dcap(expectations.pin),
        "native-QVL Intel DCAP metadata does not match the exact V1 contract"
    );
    if expectations.check_packages {
        verify_installed_packages(dcap, expectations.artifacts, expectations.build_inputs)?;
    }
    verify_entries(manifest, root, "build_inputs", expectations.build_inputs)?;
    verify_entries(manifest, root, "artifacts", expectations.artifacts)?;
    Ok(())
}

pub fn verify_qvl_manifest(
    manifest: &Value,
    pin: &Value,
    root: &Path,
    check_packages: bool,
) -> Result<()> {
    let artifacts = expected_artifacts(pin);
    let build_inputs = expected_build_inputs(pin);
    verify_qvl_manifest_with_expectations(
        manifest,
        root,
        &QvlExpectations {
            pin,
            artifacts: &artifacts,
            build_inputs: &build_inputs,
            check_packages,
        },
    )
}

pub fn install_verified_artifacts_with_expectations(
    manifest: &Value,
    root: &Path,
    destination: &Path,
    expectations: &QvlExpectations<'_>,
) -> Result<()> {
    verify_qvl_manifest_with_expectations(manifest, root, expectations)?;
    if destination.exists() {
        ensure!(
            destination.is_dir() && fs::read_dir(destination)?.next().is_none(),
            "native-QVL install destination must be empty"
        );
    } else {
        fs::create_dir_all(destination)?;
    }
    for artifact in manifest["artifacts"].as_array().expect("verified") {
        let configured = artifact["path"].as_str().expect("verified");
        let install_name = artifact["install_name"].as_str().expect("verified");
        let payload = fs::read(artifact_path(root, configured)?)?;
        let target = destination.join(install_name);
        fs::write(&target, &payload)?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

pub fn install_verified_artifacts(
    manifest: &Value,
    pin: &Value,
    root: &Path,
    destination: &Path,
    check_packages: bool,
) -> Result<()> {
    let artifacts = expected_artifacts(pin);
    let build_inputs = expected_build_inputs(pin);
    install_verified_artifacts_with_expectations(
        manifest,
        root,
        destination,
        &QvlExpectations {
            pin,
            artifacts: &artifacts,
            build_inputs: &build_inputs,
            check_packages,
        },
    )
}
