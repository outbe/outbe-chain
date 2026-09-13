use super::file_digest;
use super::is_lower_hex;
use super::read_canonical_json;
use super::run_output;
use super::SgxReleaseNetwork;
use super::SourceIdentity;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;

use serde_json::Value;

use std::fs;

use std::os::unix::fs::PermissionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

pub fn repository_root() -> Result<PathBuf> {
    let mut command = Command::new("git");
    command.args(["rev-parse", "--show-toplevel"]);
    let value = run_output(&mut command, "resolve repository root")?;
    fs::canonicalize(value.trim()).wrap_err("canonicalize repository root")
}

pub(super) fn require_release_checkout(repo_root: &Path, network: SgxReleaseNetwork) -> Result<()> {
    for relative in [
        network.bundle_spec_path(),
        "scripts/release/build-sgx-bundle-in-container.sh",
        "xtask/Cargo.toml",
    ] {
        if !repo_root.join(relative).is_file() {
            bail!("repository is missing SGX release input: {relative}");
        }
    }
    Ok(())
}

pub(super) fn read_elf_identity(elf_output: &Path) -> Result<SourceIdentity> {
    let manifest: Value = read_canonical_json(&elf_output.join("release-manifest.json"))?;
    let source = manifest
        .pointer("/release/source")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("ELF manifest lacks release source identity"))?;
    if source.get("tree_state").and_then(Value::as_str) != Some("clean")
        || source.get("clean_tree_policy").and_then(Value::as_str) != Some("required")
    {
        bail!("ELF manifest does not bind a required clean tree");
    }
    let source_commit = source
        .get("commit")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF manifest lacks source commit"))?
        .to_owned();
    let source_date_epoch = manifest
        .pointer("/build/source_date_epoch")
        .and_then(Value::as_i64)
        .ok_or_else(|| eyre!("ELF manifest lacks SOURCE_DATE_EPOCH"))?;
    let release_tag = manifest
        .pointer("/release/tag")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF manifest lacks release tag"))?
        .to_owned();
    let enclave = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .and_then(|artifacts| {
            artifacts.iter().find(|artifact| {
                artifact.get("name").and_then(Value::as_str) == Some("outbe-tee-enclave")
            })
        })
        .ok_or_else(|| eyre!("ELF manifest lacks the production enclave subject"))?;
    if enclave.get("tee") != Some(&serde_json::json!({"mock": false, "stage": "unsigned-bare-elf"}))
    {
        bail!("ELF manifest lacks the production enclave subject");
    }
    let enclave_path = elf_output.join("bin/outbe-tee-enclave");
    let metadata = fs::symlink_metadata(&enclave_path)
        .wrap_err("read enclave ELF from reproducible output")?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("reproducible output contains an unsafe enclave ELF");
    }
    let expected_digest = enclave
        .pointer("/digest/value")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("ELF manifest lacks enclave digest"))?;
    let expected_size = enclave
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| eyre!("ELF manifest lacks enclave size"))?;
    if file_digest(&enclave_path)?.value != expected_digest || metadata.len() != expected_size {
        bail!("enclave ELF does not match its release manifest");
    }
    let identity = SourceIdentity {
        release_tag,
        source_commit,
        source_date_epoch,
    };
    validate_source_identity(&identity)?;
    Ok(identity)
}

pub(super) fn validate_source_identity(identity: &SourceIdentity) -> Result<()> {
    if !is_lower_hex(&identity.source_commit, 40) {
        bail!("source commit must be a lowercase 40-character Git SHA");
    }
    if identity.source_date_epoch < 0 {
        bail!("SOURCE_DATE_EPOCH must be non-negative");
    }
    if identity.release_tag.is_empty() || !identity.release_tag.is_ascii() {
        bail!("release tag must be non-empty ASCII");
    }
    Ok(())
}

pub(super) fn require_clean_source(repo_root: &Path, expected_commit: &str) -> Result<()> {
    let mut status = Command::new("git");
    status
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"]);
    if !run_output(&mut status, "inspect source tree state")?.is_empty() {
        bail!("SGX release operations require a clean source tree");
    }
    let mut head = Command::new("git");
    head.arg("-C").arg(repo_root).args(["rev-parse", "HEAD"]);
    let head = run_output(&mut head, "resolve source commit")?;
    if head.trim() != expected_commit {
        bail!(
            "SGX source identity {expected_commit} does not match checkout {}",
            head.trim()
        );
    }
    Ok(())
}

pub(super) fn validate_signing_key(key_file: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(key_file)
        .wrap_err_with(|| format!("read signing key metadata: {}", key_file.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("missing or unsafe SGX signing key: {}", key_file.display());
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!("unsafe SGX signing key permissions: {mode:03o}; expected no group/other access");
    }
    if metadata.len() == 0 {
        bail!("SGX signing key is empty");
    }
    Ok(())
}

pub(super) fn create_empty_output(repo_root: &Path, output: &Path) -> Result<PathBuf> {
    let output = absolute_path(output)?;
    if output.starts_with(repo_root) {
        bail!("output directory must be outside the source checkout");
    }
    if output.exists() {
        if !output.is_dir() {
            bail!("output path is not a directory: {}", output.display());
        }
        if fs::read_dir(&output)
            .wrap_err("read output directory")?
            .next()
            .is_some()
        {
            bail!("output directory must be empty: {}", output.display());
        }
    } else {
        fs::create_dir_all(&output)
            .wrap_err_with(|| format!("create output directory: {}", output.display()))?;
    }
    fs::canonicalize(&output).wrap_err("canonicalize output directory")
}

pub(super) fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .wrap_err("resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes filesystem root: {}", path.display());
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}
