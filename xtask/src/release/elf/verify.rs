//! Two-builder reproducible ELF output comparison and evidence generation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{ensure, eyre, Result};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use super::manifest::{
    validate_release_manifest_v1, verify_version_text, EXPECTED_ARTIFACTS,
    PRODUCTION_TEE_FORBIDDEN_MARKERS,
};
use crate::release::sgx::canonical_json;

pub const EXPECTED_CHECKSUM_PATHS: &[&str] = &[
    "bin/outbe-chain",
    "bin/outbe-cli",
    "bin/outbe-feeder",
    "bin/outbe-keygen",
    "bin/outbe-radicle",
    "bin/outbe-tee-enclave",
    "metadata/builder-system-packages.txt",
    "metadata/outbe-chain.version.txt",
    "metadata/release-manifest-v1.schema.json",
    "metadata/reproducible-elf-build-v1.json",
    "release-manifest.json",
];

const FORBIDDEN_PATHS: &[&[u8]] = &[b"/workspace", b"/usr/local/cargo", b"/usr/local/rustup"];

fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn opt_sha256(data: &[u8]) -> Option<String> {
    (!data.is_empty()).then(|| sha256_bytes(data))
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink())
}

fn safe_material_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = Path::new(relative);
    let root_canon = (!candidate.is_absolute()).then(|| fs::canonicalize(root).ok())??;
    let joined = root_canon.join(candidate);
    let resolved = fs::canonicalize(&joined).ok()?;
    (resolved.starts_with(&root_canon) && is_regular_file(&joined)).then_some(resolved)
}

fn verify_material(root: &Path, record: &Value, label: &str, differences: &mut Vec<String>) {
    let relative = record.get("path").and_then(Value::as_str).unwrap_or("");
    let Some(path) = safe_material_path(root, relative) else {
        differences.push(format!("{label}: missing or unsafe material path: {relative}"));
        return;
    };
    let raw = fs::read(&path).unwrap_or_default();
    if record.get("size").and_then(Value::as_u64) != Some(raw.len() as u64) {
        differences.push(format!("{label}: material size mismatch: {relative}"));
    }
    if record.pointer("/digest/value").and_then(Value::as_str) != Some(sha256_bytes(&raw).as_str())
    {
        differences.push(format!("{label}: material digest mismatch: {relative}"));
    }
}

fn verify_manifest_materials(
    output: &Path,
    manifest: Option<&Value>,
    repo_root: &Path,
    label: &str,
    differences: &mut Vec<String>,
) {
    let Some(manifest) = manifest else {
        return;
    };
    let input_label = format!("{label} source input");
    for record in manifest
        .get("inputs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        verify_material(repo_root, record, &input_label, differences);
    }
    match manifest.pointer("/build/builder/resolved_system_packages") {
        Some(pkg) => verify_material(
            output,
            pkg,
            &format!("{label} package inventory"),
            differences,
        ),
        None => differences.push(format!("{label}: missing resolved system package material")),
    }
}

fn parse_checksum_lines(raw: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(raw).map_err(|_| eyre!("SHA256SUMS is not ASCII/UTF-8"))?;
    let mut declared = BTreeMap::new();
    for line in text.lines() {
        let (digest, relative) = line
            .split_once("  ")
            .ok_or_else(|| eyre!("invalid line format: {line}"))?;
        let valid_hex = digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        ensure!(valid_hex, "invalid digest for {relative}");
        ensure!(
            declared
                .insert(relative.to_owned(), digest.to_owned())
                .is_none(),
            "duplicate path: {relative}"
        );
    }
    Ok(declared)
}

fn verify_checksum_file(output: &Path, label: &str, differences: &mut Vec<String>) -> Vec<u8> {
    let checksum_path = output.join("SHA256SUMS");
    if !is_regular_file(&checksum_path) {
        differences.push(format!("{label}: missing regular SHA256SUMS"));
        return Vec::new();
    }
    let raw = fs::read(&checksum_path).unwrap_or_default();
    let Ok(declared) = parse_checksum_lines(&raw).map_err(|err| {
        differences.push(format!("{label}: invalid SHA256SUMS: {err}"));
    }) else {
        return raw;
    };
    let expected_set: BTreeSet<&str> = EXPECTED_CHECKSUM_PATHS.iter().copied().collect();
    let actual_set: BTreeSet<&str> = declared.keys().map(String::as_str).collect();
    if actual_set != expected_set {
        differences.push(format!(
            "{label}: SHA256SUMS does not declare the exact output matrix"
        ));
    }
    for (relative, expected) in &declared {
        match safe_material_path(output, relative) {
            Some(p) if sha256_bytes(&fs::read(&p).unwrap_or_default()) == *expected => {}
            Some(_) => differences.push(format!("{label}: checksum mismatch: {relative}")),
            None => differences.push(format!("{label}: checksum path missing or unsafe: {relative}")),
        }
    }
    raw
}

fn read_manifest(
    output: &Path,
    label: &str,
    differences: &mut Vec<String>,
) -> (Option<Value>, Vec<u8>) {
    let path = output.join("release-manifest.json");
    if !is_regular_file(&path) {
        differences.push(format!("{label}: missing release-manifest.json"));
        return (None, Vec::new());
    }
    let raw = fs::read(&path).unwrap_or_default();
    let manifest: Value = match serde_json::from_slice(&raw)
        .map_err(eyre::Report::from)
        .and_then(|val| validate_release_manifest_v1(&val).map(|()| val))
    {
        Ok(val) => val,
        Err(err) => {
            differences.push(format!("{label}: invalid release manifest: {err}"));
            return (None, raw);
        }
    };
    if canonical_json(&manifest).ok().as_deref() != Some(raw.as_slice()) {
        differences.push(format!(
            "{label}: release manifest is not canonical outbe JSON v1"
        ));
    }
    (Some(manifest), raw)
}

fn verify_version_evidence(
    output: &Path,
    manifest: Option<&Value>,
    label: &str,
    differences: &mut Vec<String>,
) -> Vec<u8> {
    let path = output.join("metadata/outbe-chain.version.txt");
    if !is_regular_file(&path) {
        differences.push(format!("{label}: missing regular outbe-chain version evidence"));
        return Vec::new();
    }
    let raw = fs::read(&path).unwrap_or_default();
    let Some(m) = manifest else {
        return raw;
    };
    let Ok(text) = std::str::from_utf8(&raw) else {
        differences.push(format!("{label}: version evidence is not UTF-8"));
        return raw;
    };
    let str_at = |ptr: &str| m.pointer(ptr).and_then(Value::as_str).unwrap_or_default();
    let epoch = m
        .pointer("/build/source_date_epoch")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    for diff in verify_version_text(
        text,
        str_at("/release/source/commit"),
        epoch,
        str_at("/build/target"),
        str_at("/build/profile"),
    ) {
        differences.push(format!("{label}: version identity mismatch: {diff}"));
    }
    raw
}

fn verify_elf_content(raw: &[u8], label: &str, name: &str, differences: &mut Vec<String>) {
    if !raw.starts_with(b"\x7fELF") {
        differences.push(format!("{label}: {name} is not an ELF file"));
    }
    for &forbidden in FORBIDDEN_PATHS {
        if raw.windows(forbidden.len()).any(|w| w == forbidden) {
            let text = std::str::from_utf8(forbidden).unwrap_or("<path>");
            differences.push(format!(
                "{label}: forbidden absolute build path in {name}: {text}"
            ));
        }
    }
    let markers = if name == "outbe-tee-enclave" {
        PRODUCTION_TEE_FORBIDDEN_MARKERS
    } else {
        &[]
    };
    for &marker in markers {
        if raw.windows(marker.len()).any(|w| w == marker) {
            let text = std::str::from_utf8(marker).unwrap_or("<marker>");
            differences.push(format!("{label}: forbidden production TEE marker: {text}"));
        }
    }
}

fn inspect_single_elf(
    output: &Path,
    records: &BTreeMap<String, Value>,
    label: &str,
    name: &str,
    differences: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let path = output.join("bin").join(name);
    if !is_regular_file(&path) {
        differences.push(format!("{label}: missing regular ELF: {name}"));
        return None;
    }
    let raw = fs::read(&path).unwrap_or_default();
    verify_elf_content(&raw, label, name, differences);
    if let Some(rec) = records.get(name) {
        if rec.pointer("/digest/value").and_then(Value::as_str) != Some(sha256_bytes(&raw).as_str())
        {
            differences.push(format!("{label}: manifest digest mismatch for {name}"));
        }
        if rec.get("size").and_then(Value::as_u64) != Some(raw.len() as u64) {
            differences.push(format!("{label}: manifest size mismatch for {name}"));
        }
    }
    Some(raw)
}

fn compare_artifact(
    builders: [(&Path, &BTreeMap<String, Value>); 2],
    name: &str,
    differences: &mut Vec<String>,
) -> Value {
    let raw_a = inspect_single_elf(builders[0].0, builders[0].1, "builder-a", name, differences);
    let raw_b = inspect_single_elf(builders[1].0, builders[1].1, "builder-b", name, differences);
    let identical = raw_a.as_deref().unwrap_or_default() == raw_b.as_deref().unwrap_or_default();
    if !identical {
        differences.push(format!("ELF bytes differ between builders: {name}"));
    }
    let size_a = raw_a.as_ref().map(|b| b.len() as u64);
    let size_b = raw_b.as_ref().map(|b| b.len() as u64);
    json!({
        "builder_a_sha256": raw_a.as_deref().map(sha256_bytes),
        "builder_b_sha256": raw_b.as_deref().map(sha256_bytes),
        "byte_identical": identical,
        "name": name,
        "size": (size_a == size_b).then_some(size_a).flatten(),
    })
}

fn check_git_clean_head(repo_root: &Path, expected_commit: &str, differences: &mut Vec<String>) {
    let head = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output();
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output();
    let (Ok(h), Ok(s)) = (head, status) else {
        differences.push("source identity check failed".to_owned());
        return;
    };
    if !h.status.success() || !s.status.success() {
        differences.push("source identity check failed".to_owned());
        return;
    }
    let head_str = String::from_utf8_lossy(&h.stdout).trim().to_owned();
    if head_str != expected_commit {
        differences.push(format!(
            "source checkout HEAD {head_str} does not match manifest {expected_commit}"
        ));
    }
    if !s.stdout.is_empty() {
        differences.push("source checkout is dirty during independent verification".to_owned());
    }
}

fn build_identity(
    manifest: Option<&Value>,
    repo_root: &Path,
    check_git_identity: bool,
    differences: &mut Vec<String>,
) -> Value {
    let Some(m) = manifest else {
        return json!({});
    };
    let source_commit = m
        .pointer("/release/source/commit")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if check_git_identity {
        check_git_clean_head(repo_root, source_commit, differences);
    }
    let ptr = |p: &str| m.pointer(p).cloned().unwrap_or(Value::Null);
    json!({
        "builder_base_images": ptr("/build/builder/base_images"),
        "builder_recipe": ptr("/build/builder/recipe"),
        "profile": ptr("/build/profile"),
        "release_tag": ptr("/release/tag"),
        "source_commit": source_commit,
        "source_date_epoch": ptr("/build/source_date_epoch"),
        "target": ptr("/build/target"),
    })
}

fn manifest_artifact_records(manifest: Option<&Value>) -> (Vec<String>, BTreeMap<String, Value>) {
    let mut names = Vec::new();
    let mut map = BTreeMap::new();
    for item in manifest
        .and_then(|m| m.get("artifacts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            names.push(name.to_owned());
            map.insert(name.to_owned(), item.clone());
        }
    }
    (names, map)
}

pub fn verify_outputs(
    first: &Path,
    second: &Path,
    repo_root: &Path,
    check_git_identity: bool,
) -> Result<Value> {
    let first = fs::canonicalize(first)?;
    let second = fs::canonicalize(second)?;
    let repo_root = fs::canonicalize(repo_root)?;
    let mut differences = Vec::new();

    let (first_manifest, first_manifest_raw) = read_manifest(&first, "builder-a", &mut differences);
    let (second_manifest, second_manifest_raw) =
        read_manifest(&second, "builder-b", &mut differences);
    if first_manifest_raw != second_manifest_raw {
        differences.push("release-manifest.json differs between builders".to_owned());
    }

    let (first_names, first_records) = manifest_artifact_records(first_manifest.as_ref());
    let (second_names, second_records) = manifest_artifact_records(second_manifest.as_ref());
    for (label, names) in [("builder-a", &first_names), ("builder-b", &second_names)] {
        if names != EXPECTED_ARTIFACTS {
            differences.push(format!(
                "{label} manifest does not declare the exact production ELF release matrix"
            ));
        }
    }

    verify_manifest_materials(
        &first,
        first_manifest.as_ref(),
        &repo_root,
        "builder-a",
        &mut differences,
    );
    verify_manifest_materials(
        &second,
        second_manifest.as_ref(),
        &repo_root,
        "builder-b",
        &mut differences,
    );

    let checksum_a = verify_checksum_file(&first, "builder-a", &mut differences);
    let checksum_b = verify_checksum_file(&second, "builder-b", &mut differences);
    if checksum_a != checksum_b {
        differences.push("SHA256SUMS differs between builders".to_owned());
    }

    let version_a = verify_version_evidence(
        &first,
        first_manifest.as_ref(),
        "builder-a",
        &mut differences,
    );
    let version_b = verify_version_evidence(
        &second,
        second_manifest.as_ref(),
        "builder-b",
        &mut differences,
    );
    if version_a != version_b {
        differences.push("outbe-chain version evidence differs between builders".to_owned());
    }

    let builders = [(&first as &Path, &first_records), (&second, &second_records)];
    let artifact_evidence: Vec<Value> = EXPECTED_ARTIFACTS
        .iter()
        .map(|name| compare_artifact(builders, name, &mut differences))
        .collect();

    let identity = build_identity(
        first_manifest.as_ref(),
        &repo_root,
        check_git_identity,
        &mut differences,
    );
    let cargo_lock_bytes = fs::read(repo_root.join("Cargo.lock")).unwrap_or_default();

    Ok(json!({
        "artifacts": artifact_evidence,
        "build_identity": identity,
        "checks": [
            "manifest-schema-draft-2020-12",
            "manifest-canonical-bytes",
            "exact-production-elf-matrix",
            "per-artifact-manifest-digest-and-size",
            "source-input-manifest-digest-and-size",
            "resolved-package-inventory-digest-and-size",
            "exact-output-checksum-matrix",
            "elf-magic",
            "forbidden-absolute-build-paths",
            "forbidden-production-tee-markers",
            "pinned-verifier-environment",
            "outbe-chain-version-identity",
            "byte-for-byte-two-builder-comparison",
        ],
        "differences": differences,
        "manifest": {
            "builder_a_sha256": opt_sha256(&first_manifest_raw),
            "builder_b_sha256": opt_sha256(&second_manifest_raw),
            "byte_identical": first_manifest_raw == second_manifest_raw,
        },
        "output_checksums": {
            "builder_a_sha256": opt_sha256(&checksum_a),
            "builder_b_sha256": opt_sha256(&checksum_b),
            "byte_identical": checksum_a == checksum_b,
        },
        "result": if differences.is_empty() { "passed" } else { "failed" },
        "schema_version": "1.0.0",
        "verifier": {
            "cargo_lock_sha256": sha256_bytes(&cargo_lock_bytes),
            "implementation": "cargo-xtask-release-elf-compare-v1",
        },
    }))
}
