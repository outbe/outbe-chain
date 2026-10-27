//! Validate the reproducible ELF build spec and resolve immutable release identity.

use std::fs;
use std::path::Path;
use std::process::Command;

use eyre::{ensure, eyre, Result, WrapErr as _};
use serde_json::Value;

use super::manifest::{validate_build_spec, EXPECTED_ARTIFACTS};
use crate::release::toolchain::verify_repository;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseIdentity {
    pub release_tag: String,
    pub source_commit: String,
    pub source_date_epoch: String,
    pub source_describe: String,
}

pub fn load_and_validate_build_spec(path: &Path, repo_root: Option<&Path>) -> Result<Value> {
    let raw = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let build_spec: Value =
        serde_json::from_str(&raw).wrap_err_with(|| format!("parse {}", path.display()))?;
    validate_build_spec(&build_spec)?;

    let artifacts = build_spec
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("missing artifacts array"))?;
    let names: Vec<&str> = artifacts
        .iter()
        .filter_map(|a| a.get("name").and_then(Value::as_str))
        .collect();
    ensure!(
        names == EXPECTED_ARTIFACTS,
        "build spec must declare the exact production ELF release matrix"
    );

    if let Some(root) = repo_root {
        verify_repository(root)?;
    }
    Ok(build_spec)
}

pub fn artifact_build_plan(build_spec: &Value) -> Result<Vec<Vec<String>>> {
    let target = build_spec["target"]
        .as_str()
        .ok_or_else(|| eyre!("missing target"))?;
    let common = vec![
        "cargo".to_owned(),
        "build".to_owned(),
        "--locked".to_owned(),
        "--release".to_owned(),
        "--no-default-features".to_owned(),
        "--target".to_owned(),
        target.to_owned(),
    ];
    let artifacts = build_spec["artifacts"]
        .as_array()
        .ok_or_else(|| eyre!("missing artifacts"))?;

    let mut cohorts: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for artifact in artifacts {
        let features: Vec<String> = artifact["features"]
            .as_array()
            .ok_or_else(|| eyre!("missing artifact features"))?
            .iter()
            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
            .collect();
        let pkg = artifact["package"]
            .as_str()
            .ok_or_else(|| eyre!("missing package"))?;
        let bin = artifact["name"]
            .as_str()
            .ok_or_else(|| eyre!("missing name"))?;
        let cmd = if let Some((_, existing)) = cohorts.iter_mut().find(|(f, _)| *f == features) {
            existing
        } else {
            cohorts.push((features, common.clone()));
            &mut cohorts.last_mut().expect("pushed").1
        };
        cmd.extend([
            "--package".to_owned(),
            pkg.to_owned(),
            "--bin".to_owned(),
            bin.to_owned(),
        ]);
    }
    for (features, cmd) in &mut cohorts {
        if !features.is_empty() {
            cmd.push("--features".to_owned());
            cmd.push(features.join(","));
        }
    }
    Ok(cohorts.into_iter().map(|(_, cmd)| cmd).collect())
}

pub fn artifact_check_plan(build_spec: &Value) -> Result<Vec<Vec<String>>> {
    let mut plan = artifact_build_plan(build_spec)?;
    for cmd in &mut plan {
        if let Some(sub) = cmd.get_mut(1) {
            *sub = "check".to_owned();
        }
    }
    Ok(plan)
}

fn run_plan(plan: Vec<Vec<String>>) -> Result<()> {
    for cmd in plan {
        let (program, args) = cmd
            .split_first()
            .ok_or_else(|| eyre!("empty command in plan"))?;
        let status = Command::new(program)
            .args(args)
            .status()
            .wrap_err_with(|| format!("spawn {program}"))?;
        ensure!(status.success(), "command failed: {}", cmd.join(" "));
    }
    Ok(())
}

pub fn execute_artifact_build_plan(build_spec: &Value) -> Result<()> {
    run_plan(artifact_build_plan(build_spec)?)
}

pub fn execute_artifact_check_plan(build_spec: &Value) -> Result<()> {
    run_plan(artifact_check_plan(build_spec)?)
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .wrap_err("invoke git")?;
    ensure!(out.status.success(), "git {:?} failed", args);
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

pub fn resolve_release_identity(
    repo_root: &Path,
    requested_tag: Option<&str>,
) -> Result<ReleaseIdentity> {
    let source_commit = git_output(repo_root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let source_date_epoch = git_output(repo_root, &["show", "-s", "--format=%ct", "HEAD"])?;
    let commit_identity = format!("commit-{source_commit}");
    let release_tag = match requested_tag {
        Some(tag) if !tag.is_empty() => tag.to_owned(),
        _ => commit_identity.clone(),
    };

    ensure!(
        release_tag.bytes().all(|b| (0x20..=0x7e).contains(&b)),
        "release tag must contain printable ASCII only"
    );
    if release_tag != commit_identity {
        let tag_ref = format!("refs/tags/{release_tag}^{{commit}}");
        let tag_commit = git_output(repo_root, &["rev-parse", "--verify", &tag_ref])
            .map_err(|_| eyre!("release tag does not resolve to a commit: {release_tag}"))?;
        ensure!(
            tag_commit == source_commit,
            "release tag {release_tag} resolves to {tag_commit}, not HEAD {source_commit}"
        );
    }

    Ok(ReleaseIdentity {
        source_describe: release_tag.clone(),
        release_tag,
        source_commit,
        source_date_epoch,
    })
}

pub fn resolved_values(build_spec: &Value, identity: &ReleaseIdentity) -> Result<Vec<String>> {
    let env = build_spec
        .get("environment")
        .ok_or_else(|| eyre!("missing environment"))?;
    let rustflags: Vec<&str> = env["rustflags"]
        .as_array()
        .ok_or_else(|| eyre!("missing rustflags"))?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let cflags = env["cflags"]
        .as_str()
        .ok_or_else(|| eyre!("missing cflags"))?;
    let cxxflags = env["cxxflags"]
        .as_str()
        .ok_or_else(|| eyre!("missing cxxflags"))?;
    Ok(vec![
        rustflags.join(" "),
        cflags.to_owned(),
        cxxflags.to_owned(),
        identity.source_commit.clone(),
        identity.source_date_epoch.clone(),
        identity.release_tag.clone(),
        identity.source_describe.clone(),
    ])
}
