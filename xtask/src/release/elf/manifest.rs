//! Canonical v1 Outbe ReleaseManifest generation and version output verification.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use eyre::{ensure, eyre, Result, WrapErr as _};
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;

pub const EXPECTED_ARTIFACTS: &[&str] = &[
    "outbe-up",
    "outbe-chain",
    "outbe-cli",
    "outbe-keygen",
    "outbe-feeder",
    "outbe-radicle",
];

#[derive(Clone, Debug)]
pub struct ReleaseManifestOptions {
    pub release_tag: String,
    pub source_commit: String,
    pub source_date_epoch: i64,
    pub lifecycle: String,
    pub verification_gates: Vec<Value>,
    pub resolved_system_packages: Option<PathBuf>,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).wrap_err_with(|| format!("open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(hex::encode(digest.finalize()))
}

pub fn expected_timestamp(source_date_epoch: i64) -> Result<String> {
    let dt = OffsetDateTime::from_unix_timestamp(source_date_epoch)
        .map_err(|err| eyre!("invalid SOURCE_DATE_EPOCH {source_date_epoch}: {err}"))?;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000000000Z",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
    ))
}

pub fn verify_version_text(
    text: &str,
    source_commit: &str,
    source_date_epoch: i64,
    target: &str,
    profile: &str,
) -> Vec<String> {
    let mut differences = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let expected_commit = format!("Commit SHA: {source_commit}");
    let expected_time = match expected_timestamp(source_date_epoch) {
        Ok(ts) => format!("Build Timestamp: {ts}"),
        Err(err) => {
            differences.push(err.to_string());
            return differences;
        }
    };
    let expected_profile = format!("Build Profile: {profile} ({target})");

    if text.matches(&expected_commit).count() != 2 {
        differences.push(format!(
            "expected two exact commit lines: {expected_commit}"
        ));
    }
    if text.matches(&expected_time).count() != 2 {
        differences.push(format!(
            "expected two exact deterministic timestamp lines: {expected_time}"
        ));
    }
    if !text.contains(&expected_profile) {
        differences.push(format!(
            "missing exact Outbe target/profile line: {expected_profile}"
        ));
    }
    // Vergen lowercases the CARGO_FEATURE_NATIVE_DCAP environment variable name.
    let expected_features = "Build Features: native_dcap";
    if lines.iter().filter(|&&l| l == expected_features).count() != 1 {
        differences.push(format!(
            "expected one exact Outbe feature line: {expected_features}"
        ));
    }
    differences
}

fn is_pinned_image(image: &str) -> bool {
    let Some((prefix, digest)) = image.split_once("@sha256:") else {
        return false;
    };
    let valid_prefix =
        !prefix.is_empty() && !prefix.contains('@') && !prefix.chars().any(char::is_whitespace);
    valid_prefix && is_lower_hex_n(digest, 64)
}

fn is_lower_git_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn validate_build_spec(build_spec: &Value) -> Result<()> {
    ensure!(
        build_spec.get("spec_version").and_then(Value::as_i64) == Some(1),
        "unsupported reproducible ELF build spec version"
    );
    ensure!(
        build_spec.get("target").and_then(Value::as_str) == Some("x86_64-unknown-linux-gnu"),
        "unsupported release target"
    );
    ensure!(
        build_spec.get("profile").and_then(Value::as_str) == Some("release"),
        "the v1 release recipe must preserve the release profile"
    );
    ensure!(
        build_spec.get("rust_toolchain").and_then(Value::as_str) == Some("1.96.0"),
        "the v1 release recipe requires Rust 1.96.0"
    );

    let builder = build_spec
        .get("builder")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("missing builder object"))?;
    let base_images = builder
        .get("base_images")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("builder base images must be two unique sha256-pinned images"))?;
    let unique_images: BTreeSet<&str> = base_images.iter().filter_map(Value::as_str).collect();
    ensure!(
        base_images.len() == 2
            && unique_images.len() == 2
            && unique_images.iter().copied().all(is_pinned_image),
        "builder base images must be two unique sha256-pinned images"
    );
    ensure!(
        builder.get("recipe").and_then(Value::as_str) == Some("Dockerfile.project-toolchain"),
        "builder must use the canonical project toolchain recipe"
    );
    let system_packages = builder
        .get("system_packages")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("every direct system package must have an exact version"))?;
    ensure!(
        !system_packages.is_empty()
            && system_packages.iter().all(|pkg| {
                pkg.as_str()
                    .is_some_and(|s| s.contains('=') && !s.chars().any(char::is_whitespace))
            }),
        "every direct system package must have an exact version"
    );

    let cargo = build_spec.get("cargo").and_then(Value::as_object);
    ensure!(
        cargo.and_then(|c| c.get("locked")).and_then(Value::as_bool) == Some(true),
        "release dependency resolution must be locked"
    );
    ensure!(
        cargo
            .and_then(|c| c.get("auditable"))
            .and_then(Value::as_bool)
            == Some(false),
        "recipe v1 must explicitly record cargo-auditable as disabled"
    );

    let artifacts = build_spec
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("release artifacts must have unique non-empty names"))?;
    validate_artifacts(artifacts)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum ArtifactSpecError {
    #[error("release artifacts must have unique non-empty names")]
    InvalidNames,
    #[error("release artifacts must contain the outbe-chain node")]
    MissingProductionNode,
    #[error("outbe-chain must be the production node package and binary")]
    InvalidNodeIdentity,
    #[error("production node feature set must be exactly native-dcap")]
    InvalidNodeFeatures,
    #[error("non-node release artifacts must not enable application features")]
    UnexpectedOtherFeatures,
}

fn validate_artifacts(artifacts: &[Value]) -> Result<(), ArtifactSpecError> {
    let mut names = BTreeSet::new();
    for artifact in artifacts {
        let name = artifact
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && names.insert(*name))
            .ok_or(ArtifactSpecError::InvalidNames)?;
        let features = artifact.get("features").and_then(Value::as_array);
        if name == "outbe-chain" {
            validate_production_node(artifact, features.map(Vec::as_slice))?;
        } else if !features.is_some_and(Vec::is_empty) {
            return Err(ArtifactSpecError::UnexpectedOtherFeatures);
        }
    }
    if !names.contains("outbe-chain") {
        return Err(ArtifactSpecError::MissingProductionNode);
    }
    Ok(())
}

fn validate_production_node(
    node: &Value,
    features: Option<&[Value]>,
) -> Result<(), ArtifactSpecError> {
    if node.get("role").and_then(Value::as_str) != Some("node")
        || node.get("package").and_then(Value::as_str) != Some("outbe-chain")
        || node.get("classification").and_then(Value::as_str) != Some("production")
    {
        return Err(ArtifactSpecError::InvalidNodeIdentity);
    }
    if !features.is_some_and(|f| f.len() == 1 && f[0].as_str() == Some("native-dcap")) {
        return Err(ArtifactSpecError::InvalidNodeFeatures);
    }
    Ok(())
}

fn safe_input(source_root: &Path, relative: &str) -> Result<PathBuf> {
    let candidate = Path::new(relative);
    ensure!(
        !candidate.is_absolute(),
        "input path escapes source root: {relative}"
    );
    let root = fs::canonicalize(source_root)
        .wrap_err_with(|| format!("canonicalize {}", source_root.display()))?;
    let joined = root.join(candidate);
    let resolved = fs::canonicalize(&joined)
        .map_err(|_| eyre!("input path escapes source root or missing: {relative}"))?;
    ensure!(
        resolved.starts_with(&root),
        "input path escapes source root: {relative}"
    );
    let meta = fs::symlink_metadata(&joined)
        .map_err(|_| eyre!("missing or non-regular release input: {relative}"))?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "missing or non-regular release input: {relative}"
    );
    Ok(resolved)
}

fn artifact_record(spec: &Value, artifact_dir: &Path, target: &str) -> Result<Value> {
    let name = spec["name"]
        .as_str()
        .ok_or_else(|| eyre!("artifact missing name"))?;
    let path = artifact_dir.join(name);
    let meta =
        fs::symlink_metadata(&path).map_err(|_| eyre!("missing release artifact: {name}"))?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "missing release artifact: {name}"
    );

    let mut features: Vec<String> = spec
        .get("features")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    features.sort();

    let mut install_profiles: Vec<String> = spec["install_profiles"]
        .as_array()
        .ok_or_else(|| eyre!("missing install_profiles"))?
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    install_profiles.sort();

    let mut record = Map::new();
    record.insert("classification".to_owned(), spec["classification"].clone());
    record.insert(
        "digest".to_owned(),
        json!({"algorithm": "sha256", "value": sha256_file(&path)?}),
    );
    record.insert("features".to_owned(), json!(features));
    record.insert("install_profiles".to_owned(), json!(install_profiles));
    record.insert("kind".to_owned(), json!("elf"));
    record.insert("media_type".to_owned(), json!("application/vnd.outbe.elf"));
    record.insert("name".to_owned(), json!(name));
    record.insert(
        "network_compatibility".to_owned(),
        json!("network-manifest-required"),
    );
    record.insert("package".to_owned(), spec["package"].clone());
    record.insert("path".to_owned(), json!(format!("bin/{name}")));
    record.insert(
        "platform".to_owned(),
        json!({"architecture": "x86_64", "os": "linux", "target": target}),
    );
    record.insert("role".to_owned(), spec["role"].clone());
    record.insert("size".to_owned(), json!(meta.len()));
    Ok(Value::Object(record))
}

pub fn build_manifest(
    build_spec: &Value,
    source_root: &Path,
    artifact_dir: &Path,
    options: &ReleaseManifestOptions,
) -> Result<Value> {
    validate_build_spec(build_spec)?;
    ensure!(
        !options.release_tag.is_empty() && options.release_tag.is_ascii(),
        "release tag must be non-empty ASCII"
    );
    ensure!(
        is_lower_git_sha(&options.source_commit),
        "source commit must be a lowercase 40-character Git SHA"
    );
    ensure!(
        options.source_date_epoch >= 0,
        "SOURCE_DATE_EPOCH must be a non-negative integer"
    );
    ensure!(
        matches!(
            options.lifecycle.as_str(),
            "build-candidate" | "verified" | "revoked"
        ),
        "unsupported release lifecycle"
    );

    let source_root = fs::canonicalize(source_root)?;
    let artifact_dir = fs::canonicalize(artifact_dir)?;

    let mut input_paths: Vec<String> = build_spec["inputs"]
        .as_array()
        .ok_or_else(|| eyre!("missing inputs array"))?
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    input_paths.sort();

    let mut inputs = Vec::with_capacity(input_paths.len());
    for relative in &input_paths {
        let path = safe_input(&source_root, relative)?;
        inputs.push(json!({
            "digest": {"algorithm": "sha256", "value": sha256_file(&path)?},
            "media_type": "application/octet-stream",
            "path": relative,
            "size": fs::metadata(&path)?.len(),
        }));
    }

    let target = build_spec["target"]
        .as_str()
        .ok_or_else(|| eyre!("missing target"))?;
    let spec_artifacts = build_spec["artifacts"]
        .as_array()
        .ok_or_else(|| eyre!("missing artifacts"))?;
    let mut artifacts = Vec::with_capacity(spec_artifacts.len());
    for spec in spec_artifacts {
        artifacts.push(artifact_record(spec, &artifact_dir, target)?);
    }

    let mut sys_pkgs: Vec<String> = build_spec["builder"]["system_packages"]
        .as_array()
        .ok_or_else(|| eyre!("missing system_packages"))?
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();
    sys_pkgs.sort();

    let mut builder = json!({
        "base_images": build_spec["builder"]["base_images"],
        "id": build_spec["builder"]["id"],
        "recipe": build_spec["builder"]["recipe"],
        "system_packages": sys_pkgs,
    });
    if let Some(pkg_path) = &options.resolved_system_packages {
        let meta = fs::symlink_metadata(pkg_path)
            .map_err(|_| eyre!("missing resolved system package inventory"))?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "missing resolved system package inventory"
        );
        let resolved_pkg = fs::canonicalize(pkg_path)?;
        builder.as_object_mut().expect("object").insert(
            "resolved_system_packages".to_owned(),
            json!({
                "digest": {"algorithm": "sha256", "value": sha256_file(&resolved_pkg)?},
                "media_type": "text/plain",
                "path": "metadata/builder-system-packages.txt",
                "size": meta.len(),
            }),
        );
    }

    let cargo_packages: Vec<Value> = spec_artifacts
        .iter()
        .map(|a| a["package"].clone())
        .collect();

    Ok(json!({
        "$schema": "https://outbe.io/schemas/release-manifest-v1.json",
        "artifacts": artifacts,
        "build": {
            "builder": builder,
            "cargo": {
                "auditable": build_spec["cargo"]["auditable"],
                "locked": true,
                "packages": cargo_packages,
            },
            "environment": {
                "cflags": build_spec["environment"]["cflags"],
                "cxxflags": build_spec["environment"]["cxxflags"],
                "locale": build_spec["environment"]["locale"],
                "rustflags": build_spec["environment"]["rustflags"],
                "timezone": build_spec["environment"]["timezone"],
                "zero_ar_date": build_spec["environment"]["zero_ar_date"],
            },
            "profile": build_spec["profile"],
            "provenance": {
                "entrypoint": "release/scripts/reproducible-build.sh",
                "mode": "local-container",
            },
            "rust_toolchain": build_spec["rust_toolchain"],
            "source_date_epoch": options.source_date_epoch,
            "target": build_spec["target"],
        },
        "canonicalization": {
            "encoding": "UTF-8",
            "name": "outbe-canonical-json-v1",
            "non_ascii": "lowercase-hex-unicode-escape",
            "object_keys": "unicode-code-point-order",
            "trailing_newline": true,
            "whitespace": "none",
        },
        "inputs": inputs,
        "release": {
            "lifecycle": options.lifecycle,
            "source": {
                "clean_tree_policy": "required",
                "commit": options.source_commit,
                "tree_state": "clean",
            },
            "tag": options.release_tag,
        },
        "schema_version": "1.0.0",
        "verification_gates": options.verification_gates,
    }))
}

fn is_printable_ascii(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

fn is_lower_hex_n(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// `release/release-manifest-v1.schema.json` `$defs.artifact.properties.kind.enum`.
const ARTIFACT_KINDS: &[&str] = &[
    "elf",
    "archive",
    "native-package",
    "sbom",
    "oci-manifest",
    "compose-descriptor",
];

/// `release/release-manifest-v1.schema.json` `$defs.artifact.properties.role.enum`.
const ARTIFACT_ROLES: &[&str] = &[
    "node",
    "operator-cli",
    "key-generation",
    "price-feed",
    "operator-sidecar",
    "installer",
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum ArtifactRoleKindError {
    #[error("missing artifact role")]
    MissingRole,
    #[error("invalid artifact role: {0}")]
    InvalidRole(String),
    #[error("missing artifact kind")]
    MissingKind,
    #[error("invalid artifact kind: {0}")]
    InvalidKind(String),
}

fn validate_artifact_role_and_kind(art: &Value) -> std::result::Result<(), ArtifactRoleKindError> {
    let role = art
        .get("role")
        .and_then(Value::as_str)
        .ok_or(ArtifactRoleKindError::MissingRole)?;
    if !ARTIFACT_ROLES.contains(&role) {
        return Err(ArtifactRoleKindError::InvalidRole(role.into()));
    }
    let kind = art
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(ArtifactRoleKindError::MissingKind)?;
    if !ARTIFACT_KINDS.contains(&kind) {
        return Err(ArtifactRoleKindError::InvalidKind(kind.into()));
    }
    Ok(())
}

fn validate_tee_schema(tee: &Map<String, Value>) -> Result<()> {
    ensure!(
        tee.get("mock").and_then(Value::as_bool) == Some(false),
        "tee.mock must be false"
    );
    let stage = tee
        .get("stage")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing tee.stage"))?;
    ensure!(
        matches!(stage, "unsigned-bare-elf" | "rendered" | "signed"),
        "invalid tee.stage"
    );
    if stage == "signed" {
        let scope = tee
            .get("authorization_scope")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing tee.authorization_scope"))?;
        ensure!(
            matches!(scope, "mainnet" | "testnet"),
            "invalid tee.authorization_scope"
        );
        let mrenclave = tee
            .get("mrenclave")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing tee.mrenclave"))?;
        let mrsigner = tee
            .get("mrsigner")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing tee.mrsigner"))?;
        ensure!(
            is_lower_hex_n(mrenclave, 64) && is_lower_hex_n(mrsigner, 64),
            "invalid tee measurements"
        );
        let prod_id = tee
            .get("isv_prod_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("missing tee.isv_prod_id"))?;
        let svn = tee
            .get("isv_svn")
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("missing tee.isv_svn"))?;
        let schema_v = tee
            .get("sealed_state_schema")
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("missing tee.sealed_state_schema"))?;
        ensure!(
            prod_id <= 65535 && svn <= 65535 && schema_v >= 1,
            "invalid tee integer bounds"
        );
    }
    Ok(())
}

fn validate_provenance_schema(prov: &Map<String, Value>) -> Result<String> {
    ensure!(
        prov.get("entrypoint").and_then(Value::as_str)
            == Some("release/scripts/reproducible-build.sh"),
        "invalid provenance.entrypoint"
    );
    let mode = prov
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing provenance.mode"))?;
    ensure!(
        matches!(mode, "local-container" | "github-actions"),
        "invalid provenance.mode"
    );
    if mode == "github-actions" {
        let wf = prov
            .get("workflow")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing provenance.workflow"))?;
        let cid = prov
            .get("certificate_identity")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing provenance.certificate_identity"))?;
        let issuer = prov
            .get("certificate_oidc_issuer")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing provenance.certificate_oidc_issuer"))?;
        let sha = prov
            .get("certificate_workflow_sha")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("missing provenance.certificate_workflow_sha"))?;
        ensure!(
            issuer == "https://token.actions.githubusercontent.com" && is_lower_hex_n(sha, 40),
            "invalid github-actions provenance issuer or workflow sha"
        );
        let valid_pair = (wf, cid)
            == (
                ".github/workflows/release.yml",
                "https://github.com/outbe/outbe-chain/.github/workflows/release.yml@refs/heads/main",
            );
        ensure!(valid_pair, "invalid workflow/certificate_identity pair");
    }
    Ok(mode.to_owned())
}

fn validate_network_schema(net: &Map<String, Value>) -> Result<&'static str> {
    let chain_id = net
        .get("chain_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("missing network.chain_id"))?;
    let chain_name = net
        .get("chain_name")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing network.chain_name"))?;
    let ghash = net
        .get("genesis_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing network.genesis_hash"))?;
    ensure!(
        ghash.starts_with("0x") && is_lower_hex_n(&ghash[2..], 64),
        "invalid network.genesis_hash"
    );
    let gpath = net
        .get("genesis_file")
        .and_then(|g| g.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing network.genesis_file.path"))?;
    match (chain_id, chain_name, gpath) {
        (676, "outbe-mainnet-1", "mainnet-genesis.json") => Ok("mainnet"),
        (54_322_345, "outbe-testnet-1", "testnet-genesis.json") => Ok("testnet"),
        _ => Err(eyre!("invalid network identity tuple")),
    }
}

pub fn validate_release_manifest_v1(manifest: &Value) -> Result<()> {
    let obj = manifest
        .as_object()
        .ok_or_else(|| eyre!("manifest must be an object"))?;
    for req in [
        "$schema",
        "schema_version",
        "canonicalization",
        "release",
        "build",
        "inputs",
        "artifacts",
        "verification_gates",
    ] {
        ensure!(obj.contains_key(req), "missing required field: {req}");
    }
    ensure!(
        obj.get("$schema").and_then(Value::as_str)
            == Some("https://outbe.io/schemas/release-manifest-v1.json")
            && obj.get("schema_version").and_then(Value::as_str) == Some("1.0.0"),
        "invalid schema header"
    );
    let release = obj["release"]
        .as_object()
        .ok_or_else(|| eyre!("release must be object"))?;
    let lifecycle = release
        .get("lifecycle")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing release.lifecycle"))?;
    ensure!(
        matches!(lifecycle, "build-candidate" | "verified" | "revoked"),
        "invalid release.lifecycle"
    );
    let tag = release
        .get("tag")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("missing release.tag"))?;
    ensure!(is_printable_ascii(tag), "invalid release.tag");
    let prov = obj["build"]
        .get("provenance")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("missing build.provenance"))?;
    let prov_mode = validate_provenance_schema(prov)?;
    let artifacts = obj["artifacts"]
        .as_array()
        .ok_or_else(|| eyre!("artifacts must be array"))?;
    ensure!(!artifacts.is_empty(), "artifacts must not be empty");
    let mut has_signed_tee = false;
    for art in artifacts {
        validate_artifact_role_and_kind(art)?;
        if let Some(tee) = art.get("tee").and_then(Value::as_object) {
            validate_tee_schema(tee)?;
            if tee.get("stage").and_then(Value::as_str) == Some("signed") {
                has_signed_tee = true;
            }
        }
    }
    if lifecycle == "verified" {
        ensure!(
            obj.contains_key("network") && prov_mode == "github-actions",
            "verified lifecycle requires network and github-actions provenance"
        );
    }
    if has_signed_tee {
        ensure!(
            obj.contains_key("network"),
            "signed tee artifact requires network"
        );
    }
    if let Some(net_val) = obj.get("network") {
        let net = net_val
            .as_object()
            .ok_or_else(|| eyre!("network must be object"))?;
        let scope = validate_network_schema(net)?;
        for art in artifacts {
            if let Some(auth_scope) = art
                .get("tee")
                .and_then(|t| t.get("authorization_scope"))
                .and_then(Value::as_str)
            {
                ensure!(
                    auth_scope == scope,
                    "tee.authorization_scope mismatches network"
                );
            }
        }
        if let Some(wf) = prov.get("workflow").and_then(Value::as_str) {
            ensure!(
                wf == format!(".github/workflows/{scope}-release.yml"),
                "provenance.workflow mismatches network"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_allowlists_match_release_manifest_schema() {
        let schema: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../release/release-manifest-v1.schema.json"
        )))
        .unwrap();
        let properties = &schema["$defs"]["artifact"]["properties"];
        for (field, allowed) in [("kind", ARTIFACT_KINDS), ("role", ARTIFACT_ROLES)] {
            assert_eq!(properties[field]["enum"], json!(allowed), "{field} enum");
        }
    }

    fn minimal_manifest(role: &str, kind: &str) -> Value {
        json!({
            "$schema": "https://outbe.io/schemas/release-manifest-v1.json",
            "schema_version": "1.0.0",
            "canonicalization": {},
            "release": {"lifecycle": "build-candidate", "tag": "v0"},
            "build": {
                "provenance": {
                    "entrypoint": "release/scripts/reproducible-build.sh",
                    "mode": "local-container"
                }
            },
            "inputs": [],
            "artifacts": [{"role": role, "kind": kind}],
            "verification_gates": []
        })
    }

    fn assert_rejected(manifest: &Value, expected: ArtifactRoleKindError) {
        let err = validate_release_manifest_v1(manifest)
            .expect_err("invalid artifact role or kind must fail");
        assert_eq!(err.downcast_ref::<ArtifactRoleKindError>(), Some(&expected));
    }

    #[test]
    fn rejects_removed_and_unknown_artifact_role_and_kind() {
        assert!(validate_release_manifest_v1(&minimal_manifest("node", "elf")).is_ok());

        for role in [concat!("tee-", "enclave"), "not-a-role"] {
            assert_rejected(
                &minimal_manifest(role, "elf"),
                ArtifactRoleKindError::InvalidRole(role.into()),
            );
        }
        for kind in [
            concat!("sgx-", "rendered-manifest"),
            concat!("sgx-", "sigstruct"),
            concat!("gramine-", "runtime-tree"),
            "not-a-kind",
        ] {
            assert_rejected(
                &minimal_manifest("node", kind),
                ArtifactRoleKindError::InvalidKind(kind.into()),
            );
        }
        for (field, expected) in [
            ("role", ArtifactRoleKindError::MissingRole),
            ("kind", ArtifactRoleKindError::MissingKind),
        ] {
            let mut manifest = minimal_manifest("node", "elf");
            manifest["artifacts"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert_rejected(&manifest, expected);
        }
    }
}
