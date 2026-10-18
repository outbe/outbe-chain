use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

use eyre::{ensure, Result};

use super::{
    command_output, identify, lane_artifacts, publish_manifest, release_build_command, run_cargo,
    source_fingerprint, source_fingerprint_with_diff, ArtifactIdentity, BuildLane, BuildManifestV1,
    SourceFingerprintV1, BUILD_MANIFEST_SCHEMA_VERSION,
};

/// Authenticated prior inputs for a harness refresh.
#[derive(Clone, Copy, Debug)]
pub struct HarnessRefreshOptions<'a> {
    pub prior_manifest: &'a Path,
    pub prior_source_diff: Option<&'a Path>,
    pub refresh_genesis_tooling: bool,
}

pub(super) const GENESIS_TOOLING_INPUTS: [(&str, &str); 2] = [
    (
        "genesis_validator_seeder_source",
        "scripts/genesis_seed_validators.py",
    ),
    (
        "genesis_validator_seeder_layout_test",
        "scripts/test_seed_genesis_protocol_constants.py",
    ),
];

/// Rebuild the harness while retaining byte-identical release runtime artifacts.
/// The prior manifest must describe the same checkout, lockfile, toolchain, and lane.
/// A saved source diff can authenticate unchanged dirty files from the prior build.
pub fn refresh_harness_lane(
    repo: &Path,
    lane: BuildLane,
    jobs: usize,
    options: HarnessRefreshOptions<'_>,
    output: &Path,
) -> Result<()> {
    let HarnessRefreshOptions {
        prior_manifest: prior_path,
        prior_source_diff,
        refresh_genesis_tooling,
    } = options;
    ensure!((1..=8).contains(&jobs), "--jobs must be between 1 and 8");
    let repo = repo.canonicalize()?;
    let mut reuse_evidence = Vec::new();
    ensure!(
        !refresh_genesis_tooling || prior_source_diff.is_some(),
        "genesis tooling refresh requires an authenticated prior source diff"
    );
    if refresh_genesis_tooling {
        reuse_evidence.extend(genesis_tooling_evidence(&repo)?);
    }
    if let Some(diff_path) = prior_source_diff {
        for (name, path) in [
            ("reuse_prior_manifest", prior_path),
            ("reuse_prior_source_diff", diff_path),
        ] {
            reuse_evidence.push((name.to_owned(), identify(&path.canonicalize()?, false)?));
        }
    }
    for (name, identity) in &reuse_evidence {
        ensure!(
            !output.exists() || output.canonicalize()? != identity.canonical_path,
            "preserve reuse evidence {name} when publishing the new manifest"
        );
    }
    let prior: BuildManifestV1 = serde_json::from_slice(&fs::read(prior_path)?)?;
    ensure!(
        prior.schema_version == BUILD_MANIFEST_SCHEMA_VERSION,
        "prior manifest schema differs"
    );
    ensure!(prior.lane == lane, "prior manifest lane differs");
    let source_before = source_fingerprint(&repo)?;
    ensure!(
        prior.source.git_sha == source_before.git_sha,
        "checkout commit differs"
    );
    ensure!(
        prior.source.cargo_lock_sha256 == source_before.cargo_lock_sha256,
        "Cargo.lock differs"
    );
    ensure!(
        prior.source.rust_toolchain_sha256 == source_before.rust_toolchain_sha256,
        "Rust toolchain differs"
    );
    verify_reused_sources(
        &repo,
        &prior.source,
        prior_source_diff,
        refresh_genesis_tooling,
    )?;
    for spec in lane_artifacts(&repo, lane)
        .into_iter()
        .filter(|spec| spec.name != "outbe_e2e")
    {
        let expected = prior
            .artifacts
            .get(spec.name)
            .ok_or_else(|| eyre::eyre!("prior manifest lacks {}", spec.name))?;
        ensure!(
            identify(&spec.path, spec.executable)? == *expected,
            "runtime artifact {} differs from prior manifest",
            spec.name
        );
    }
    let harness_command = release_build_command(
        &jobs.to_string(),
        &["-p", "outbe-e2e-harness", "--bin", "outbe-e2e"],
    );
    run_cargo(&repo, &harness_command)?;
    ensure!(
        source_fingerprint(&repo)? == source_before,
        "source changed during harness build"
    );
    let mut artifacts = prior.artifacts;
    for spec in lane_artifacts(&repo, lane) {
        let identity = identify(&spec.path, spec.executable)?;
        if spec.name != "outbe_e2e" {
            ensure!(
                artifacts.get(spec.name) == Some(&identity),
                "runtime artifact {} changed during harness build",
                spec.name
            );
        }
        artifacts.insert(spec.name.to_owned(), identity);
    }
    let mut commands = prior.cargo_commands;
    commands.push(harness_command);
    for (name, identity) in reuse_evidence {
        ensure!(
            identify(&identity.requested_path, false)? == identity,
            "reuse evidence {name} changed during harness build"
        );
        artifacts.insert(name, identity);
    }
    publish_manifest(
        output,
        &BuildManifestV1 {
            schema_version: BUILD_MANIFEST_SCHEMA_VERSION,
            lane,
            source: source_before,
            cargo_commands: commands,
            artifacts,
        },
    )?;
    eprintln!(
        "outbe-e2e-build: refreshed harness manifest {}",
        output.display()
    );
    Ok(())
}

pub(super) fn verify_reused_sources(
    repo: &Path,
    prior: &SourceFingerprintV1,
    prior_source_diff: Option<&Path>,
    refresh_genesis_tooling: bool,
) -> Result<()> {
    ensure!(
        !refresh_genesis_tooling || prior_source_diff.is_some(),
        "genesis tooling refresh requires an authenticated prior source diff"
    );
    let Some(diff_path) = prior_source_diff else {
        for area in ["bin", "crates", "contracts", "scripts", "release"] {
            ensure!(
                command_output(repo, "git", &["status", "--porcelain", "--", area])?.is_empty(),
                "production inputs under {area} are dirty; rebuild the complete lane"
            );
        }
        return Ok(());
    };
    let diff = fs::read(diff_path)?;
    ensure!(
        source_fingerprint_with_diff(repo, &diff)? == *prior,
        "saved source diff and current untracked inputs do not match the prior manifest"
    );

    // Reconstruct the authenticated baseline without modifying the working index.
    let index = tempfile::NamedTempFile::new()?;
    let run = |arguments: &[&OsStr]| -> Result<Vec<u8>> {
        let output = Command::new("git")
            .args(arguments)
            .env("GIT_INDEX_FILE", index.path())
            .current_dir(repo)
            .output()?;
        ensure!(
            output.status.success(),
            "cannot reconstruct baseline source: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(output.stdout)
    };
    run(&[OsStr::new("read-tree"), OsStr::new(&prior.git_sha)])?;
    if !diff.is_empty() {
        let patch = tempfile::NamedTempFile::new()?;
        fs::write(patch.path(), &diff)?;
        run(&[
            OsStr::new("apply"),
            OsStr::new("--cached"),
            OsStr::new("--binary"),
            patch.path().as_os_str(),
        ])?;
    }
    let mut current_arguments = vec![
        "diff",
        "--binary",
        "HEAD",
        "--",
        ".",
        ":(exclude)testing/e2e-harness",
    ];
    let tooling_exclusions: Vec<_> = if refresh_genesis_tooling {
        GENESIS_TOOLING_INPUTS
            .iter()
            .map(|(_, path)| format!(":(exclude,literal){path}"))
            .collect()
    } else {
        Vec::new()
    };
    current_arguments.extend(tooling_exclusions.iter().map(String::as_str));
    let mut baseline_arguments: Vec<_> = current_arguments.iter().map(OsStr::new).collect();
    baseline_arguments.insert(2, OsStr::new("--cached"));
    let baseline = run(&baseline_arguments)?;
    let current = command_output(repo, "git", &current_arguments)?;
    ensure!(
        baseline == current,
        "source outside testing/e2e-harness differs from the authenticated baseline"
    );
    Ok(())
}

pub(super) fn genesis_tooling_evidence(repo: &Path) -> Result<Vec<(String, ArtifactIdentity)>> {
    GENESIS_TOOLING_INPUTS
        .iter()
        .map(|(name, relative)| {
            command_output(
                repo,
                "git",
                &["ls-files", "--error-unmatch", "--", relative],
            )?;
            Ok(((*name).to_owned(), identify(&repo.join(relative), false)?))
        })
        .collect()
}
