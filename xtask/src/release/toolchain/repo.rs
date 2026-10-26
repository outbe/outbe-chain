//! Repository consumer checks for the Outbe project toolchain pin.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use eyre::{bail, Result};
use serde_json::{json, Value};

use super::pin::{
    load_and_validate_pin, read_json_obj, APT_SNAPSHOT_HOST, APT_SOURCES_PATH, CONTRACT_PATH,
    MUTABLE_APT_HOSTS,
};

fn parse_rust_toolchain_toml(text: &str) -> (String, Vec<String>) {
    let mut channel = String::new();
    let mut components = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("channel") {
            channel = rest
                .trim_start_matches([' ', '='])
                .trim_matches('"')
                .to_owned();
        } else if let Some(rest) = trimmed.strip_prefix("components") {
            let inner = rest
                .trim_start_matches([' ', '='])
                .trim_matches(['[', ']']);
            components = inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            components.sort();
        }
    }
    (channel, components)
}

fn check_elf_and_sgx_consumers(root: &Path, pin: &Value, diffs: &mut Vec<String>) -> Vec<String> {
    let rust_ver = pin["rust"]["version"].as_str().unwrap_or("");
    let gramine_ver = pin["gramine"]["version"].as_str().unwrap_or("");
    let gramine_sha = pin["gramine"]["source_commit"].as_str().unwrap_or("");
    let pkgs = pin["system_packages"].as_object().expect("validated");

    let rt_text = fs::read_to_string(root.join("rust-toolchain.toml")).unwrap_or_default();
    let (rt_chan, rt_comps) = parse_rust_toolchain_toml(&rt_text);
    if rt_chan != rust_ver {
        diffs.push(format!(
            "rust-toolchain channel: expected {rust_ver:?}, found {rt_chan:?}"
        ));
    }
    let expected_comps: Vec<String> = pin["rust"]["components"]
        .as_array()
        .expect("validated")
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    if rt_comps != expected_comps {
        diffs.push(format!(
            "rust-toolchain components: expected {expected_comps:?}, found {rt_comps:?}"
        ));
    }

    let elf =
        read_json_obj(&root.join("release/reproducible-elf-build-v1.json")).unwrap_or_default();
    if elf.get("project_toolchain").and_then(Value::as_str) != Some(CONTRACT_PATH) {
        diffs.push("ELF project toolchain contract mismatch".to_owned());
    }
    if elf.get("rust_toolchain").and_then(Value::as_str) != Some(rust_ver) {
        diffs.push("ELF Rust version mismatch".to_owned());
    }
    let base_images: Vec<String> = elf
        .pointer("/builder/base_images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    if !base_images
        .iter()
        .any(|img| img.contains(&format!("rust:{rust_ver}-")))
    {
        diffs.push("ELF builder images do not use the pinned Rust version".to_owned());
    }
    if !base_images
        .iter()
        .any(|img| img.contains(&format!("gramineproject/gramine:{gramine_ver}-")))
    {
        diffs.push("ELF builder images do not use the pinned Gramine version".to_owned());
    }
    let expected_sys_pkgs: Vec<Value> = pkgs
        .iter()
        .map(|(k, v)| json!(format!("{k}={}", v.as_str().unwrap_or(""))))
        .collect();
    if elf.pointer("/builder/system_packages") != Some(&Value::Array(expected_sys_pkgs)) {
        diffs.push("ELF builder system packages mismatch".to_owned());
    }
    let elf_inputs: BTreeSet<&str> = elf
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for req in [
        CONTRACT_PATH,
        APT_SOURCES_PATH,
        "release/dcap-native-qvl-v1.json",
        "release/scripts/verify-toolchain-in-container.sh",
    ] {
        if !elf_inputs.contains(req) {
            diffs.push(format!("ELF release inputs do not bind {req}"));
        }
    }

    for network in ["testnet", "mainnet"] {
        let sgx = read_json_obj(&root.join(format!("release/{network}-sgx-bundle-v1.json")))
            .unwrap_or_default();
        if sgx.get("project_toolchain").and_then(Value::as_str) != Some(CONTRACT_PATH) {
            diffs.push(format!("{network} SGX project toolchain contract mismatch"));
        }
        if sgx.pointer("/gramine/version").and_then(Value::as_str) != Some(gramine_ver) {
            diffs.push(format!("{network} SGX Gramine version mismatch"));
        }
        if sgx
            .pointer("/gramine/source_commit")
            .and_then(Value::as_str)
            != Some(gramine_sha)
        {
            diffs.push(format!("{network} SGX Gramine source commit mismatch"));
        }
    }
    base_images
}

fn check_qvl_and_dockerfile(
    root: &Path,
    pin: &Value,
    elf_images: &[String],
    diffs: &mut Vec<String>,
) {
    let rust_ver = pin["rust"]["version"].as_str().unwrap_or("");
    let gramine_ver = pin["gramine"]["version"].as_str().unwrap_or("");
    let pkgs = pin["system_packages"].as_object().expect("validated");

    let qvl = read_json_obj(&root.join("release/dcap-native-qvl-v1.json")).unwrap_or_default();
    if qvl.get("gramine_version").and_then(Value::as_str) != Some(gramine_ver) {
        diffs.push("QVL Gramine version mismatch".to_owned());
    }
    for (pkg, ptr) in [
        (
            "libsgx-dcap-quote-verify",
            "/intel_dcap/runtime_package_version",
        ),
        (
            "libsgx-dcap-quote-verify-dev",
            "/intel_dcap/development_package_version",
        ),
        ("libsgx-headers", "/intel_dcap/headers_package_version"),
    ] {
        if qvl.pointer(ptr) != pkgs.get(pkg) {
            diffs.push(format!("QVL {pkg} version mismatch"));
        }
    }

    let recipe = fs::read_to_string(root.join("Dockerfile.project-toolchain")).unwrap_or_default();
    for (pkg, ver) in pkgs {
        let needle = format!("{pkg}={}", ver.as_str().unwrap_or(""));
        if !recipe.contains(&needle) {
            diffs.push(format!(
                "project toolchain recipe does not install {needle}"
            ));
        }
    }
    for img in elf_images {
        if !recipe.contains(&format!("FROM {img} ")) {
            diffs.push(format!("project toolchain recipe does not consume {img}"));
        }
    }
    if !recipe.contains(&format!("rust:{rust_ver}-"))
        || !recipe.contains(&format!("gramineproject/gramine:{gramine_ver}-"))
    {
        diffs.push("project toolchain recipe does not use pinned Rust/Gramine".to_owned());
    }
}

fn check_apt_sources_and_tee(root: &Path, pin: &Value, diffs: &mut Vec<String>) {
    let pkgs = pin["system_packages"].as_object().expect("validated");
    let apt_sources = fs::read_to_string(root.join(APT_SOURCES_PATH)).unwrap_or_default();
    let uris: Vec<&str> = apt_sources
        .lines()
        .filter_map(|l| l.strip_prefix("URIs:").map(str::trim))
        .collect();
    if uris.is_empty() || uris.iter().any(|u| !u.starts_with(APT_SNAPSHOT_HOST)) {
        diffs.push("frozen APT sources use a mutable or missing archive URI".to_owned());
    }
    for host in MUTABLE_APT_HOSTS {
        if apt_sources
            .lines()
            .filter(|l| !l.starts_with('#'))
            .any(|l| l.contains(host))
        {
            diffs.push(format!(
                "frozen APT sources still reach the mutable host {host}"
            ));
        }
    }

    let tee_build =
        fs::read_to_string(root.join("crates/system/tee/build.rs")).unwrap_or_default();
    for pin_inc in [
        "include_str!(\"../../../release/project-toolchain-v1.json\")",
        "include_str!(\"../../../release/dcap-native-qvl-v1.json\")",
    ] {
        if !tee_build.contains(pin_inc) {
            diffs.push(format!(
                "native-DCAP build contract does not consume {pin_inc}"
            ));
        }
    }
    for ver in pkgs.values().filter_map(Value::as_str) {
        if tee_build.contains(&format!("\"{ver}\"")) {
            diffs.push(format!(
                "native-DCAP build contract duplicates project package version {ver}"
            ));
        }
    }
}

pub fn repository_differences(repo_root: &Path) -> Result<Vec<String>> {
    let pin = load_and_validate_pin(&repo_root.join(CONTRACT_PATH))?;
    let mut diffs = Vec::new();
    let images = check_elf_and_sgx_consumers(repo_root, &pin, &mut diffs);
    check_qvl_and_dockerfile(repo_root, &pin, &images, &mut diffs);
    check_apt_sources_and_tee(repo_root, &pin, &mut diffs);
    Ok(diffs)
}

pub fn verify_repository(repo_root: &Path) -> Result<()> {
    let root = fs::canonicalize(repo_root)?;
    let diffs = repository_differences(&root)?;
    if !diffs.is_empty() {
        let detail = diffs
            .iter()
            .map(|d| format!("- {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        bail!("project toolchain version drift:\n{detail}");
    }
    Ok(())
}
