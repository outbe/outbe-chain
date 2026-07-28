//! Atomic, content-addressed evidence publication.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use eyre::{bail, ensure, Result, WrapErr};
use sha2::{Digest, Sha256};

use super::schema::{
    AssertionRecordV1, ClosureReportV1, MemberDigestV1, RunManifestV1, SourceIdentityV1,
};

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Capture clean/dirty state and exact Rust dependency inputs.
pub fn capture_source_identity(repo: &Path) -> Result<SourceIdentityV1> {
    let sha = command_stdout(repo, "git", &["rev-parse", "HEAD"])?;
    let tracked = command_output(
        repo,
        "git",
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    let untracked = command_output(repo, "git", &["ls-files", "--others", "--exclude-standard"])?;
    let metadata = command_output(
        repo,
        "cargo",
        &["metadata", "--locked", "--no-deps", "--format-version", "1"],
    )?;
    Ok(SourceIdentityV1 {
        sha,
        tracked_dirty: !tracked.is_empty(),
        untracked_dirty: !untracked.is_empty(),
        cargo_lock_sha256: hash_file(&repo.join("Cargo.lock"))?.sha256,
        rust_toolchain_sha256: hash_file(&repo.join("rust-toolchain.toml"))?.sha256,
        dependency_metadata_sha256: sha256_hex(&metadata),
    })
}

/// Publish one immutable bundle member and return its exact identity.
pub fn publish_member(root: &Path, relative: &str, bytes: &[u8]) -> Result<MemberDigestV1> {
    let target = checked_target(root, relative)?;
    atomic_publish(&target, bytes)?;
    Ok(MemberDigestV1 {
        path: relative.to_owned(),
        length: bytes
            .len()
            .try_into()
            .wrap_err("member length does not fit u64")?,
        sha256: sha256_hex(bytes),
    })
}

/// Encode and publish JSONL assertion records.
pub fn publish_assertions(
    root: &Path,
    relative: &str,
    assertions: &[AssertionRecordV1],
) -> Result<MemberDigestV1> {
    let mut bytes = Vec::new();
    for assertion in assertions {
        serde_json::to_writer(&mut bytes, assertion)?;
        bytes.push(b'\n');
    }
    publish_member(root, relative, &bytes)
}

/// Publish `run-manifest.json` after every referenced member exists.
pub fn publish_manifest(root: &Path, manifest: &RunManifestV1) -> Result<PathBuf> {
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    let path = root.join("run-manifest.json");
    atomic_publish(&path, &bytes)?;
    Ok(path)
}

/// Publish deterministic JSON/Markdown closure reports and the JSON digest.
pub fn publish_report(root: &Path, report: &ClosureReportV1) -> Result<()> {
    fs::create_dir_all(root)
        .wrap_err_with(|| format!("create report directory {}", root.display()))?;
    let mut json = serde_json::to_vec_pretty(report)?;
    json.push(b'\n');
    let markdown = render_markdown(report);
    let digest = format!("{}  closure-report.json\n", sha256_hex(&json));
    atomic_publish(&root.join("closure-report.json"), &json)?;
    atomic_publish(&root.join("closure-report.md"), markdown.as_bytes())?;
    atomic_publish(&root.join("closure-report.sha256"), digest.as_bytes())?;
    Ok(())
}

/// Recompute the exact length and SHA-256 of a file.
pub fn hash_file(path: &Path) -> Result<MemberDigestV1> {
    let bytes = fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?;
    Ok(MemberDigestV1 {
        path: path.to_string_lossy().into_owned(),
        length: bytes
            .len()
            .try_into()
            .wrap_err("file length does not fit u64")?,
        sha256: sha256_hex(&bytes),
    })
}

/// Lowercase SHA-256 hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn checked_target(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    ensure!(
        !relative_path.is_absolute(),
        "evidence member path is absolute"
    );
    ensure!(
        relative_path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "evidence member path contains traversal or prefix"
    );
    ensure!(
        relative != "run-manifest.json"
            && relative != "closure-report.json"
            && relative != "closure-report.md"
            && relative != "closure-report.sha256",
        "reserved evidence path {relative}"
    );
    Ok(root.join(relative_path))
}

fn atomic_publish(target: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        !target.exists(),
        "refusing to replace immutable evidence {}",
        target.display()
    );
    let parent = target
        .parent()
        .ok_or_else(|| eyre::eyre!("evidence target has no parent"))?;
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("create evidence directory {}", parent.display()))?;

    let temporary = temporary_path(target);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .wrap_err_with(|| format!("create temporary evidence {}", temporary.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .wrap_err_with(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .wrap_err_with(|| format!("fsync {}", temporary.display()))?;
        drop(file);
        fs::hard_link(&temporary, target).wrap_err_with(|| {
            format!(
                "link immutable evidence {} -> {} without replacement",
                temporary.display(),
                target.display()
            )
        })?;
        fs::remove_file(&temporary)
            .wrap_err_with(|| format!("remove published temporary {}", temporary.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(target: &Path) -> PathBuf {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .wrap_err_with(|| format!("fsync directory {}", path.display()))?;
    }
    Ok(())
}

fn command_stdout(repo: &Path, program: &str, args: &[&str]) -> Result<String> {
    let bytes = command_output(repo, program, args)?;
    let output =
        String::from_utf8(bytes).wrap_err_with(|| format!("{program} output is not UTF-8"))?;
    let output = output.trim();
    ensure!(!output.is_empty(), "{program} produced empty output");
    Ok(output.to_owned())
}

fn command_output(repo: &Path, program: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo)
        .output()
        .wrap_err_with(|| format!("run {program} {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn render_markdown(report: &ClosureReportV1) -> String {
    let task = report.task_id.as_deref().unwrap_or("-");
    let source = report.source_sha.as_deref().unwrap_or("-");
    format!(
        "# OCOMP PoC evidence report\n\n\
         - Mode: `{}`\n\
         - Task: `{task}`\n\
         - Source: `{source}`\n\
         - Status: **{}**\n\n\
         ## Coverage\n\n\
         - Passed tests: {}\n\
         - Missing tests: {}\n\
         - Non-PASS tests: {}\n\
         - Requirement gaps: {}\n\
         - Deferred requirements: {}\n\n\
         ## Errors\n\n{}\n",
        report.mode,
        report.status,
        list_or_none(&report.passed_test_ids),
        list_or_none(&report.missing_test_ids),
        list_or_none(&report.non_pass_test_ids),
        list_or_none(&report.requirement_gaps),
        list_or_none(&report.deferred_requirement_ids),
        if report.errors.is_empty() {
            "None.".to_owned()
        } else {
            report
                .errors
                .iter()
                .map(|error| format!("- {error}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    )
}

fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}
