//! dstack `repro-build.sh` invocation, the optional two-run reproducibility check, and packing of
//! the built flavor images into distribution archives.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

use eyre::{bail, ensure, Context, Result};
use outbe_software_bundle::sha256_file;

use crate::{archive, capture_git_in, run_git, run_git_in, Flavor, FlavorArtifact, VmImageOptions};

/// Settings that `repro-build.sh` receives for one build run.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReproBuildSettings<'a> {
    pub(crate) flavors: &'a [Flavor],
    pub(crate) jobs: usize,
    pub(crate) mkosi_image: &'a str,
    pub(crate) cpuset_cpus: Option<&'a str>,
}

impl<'a> ReproBuildSettings<'a> {
    pub(crate) fn from_options(options: &VmImageOptions<'a>) -> Self {
        Self {
            flavors: options.flavors,
            jobs: options.jobs,
            mkosi_image: options.mkosi_image,
            cpuset_cpus: options.cpuset_cpus,
        }
    }
}

/// Identity of one image build: its source commits, the dstack version, and the flavors it
/// produces.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BuildIdentity<'a> {
    pub(crate) source_commit: &'a str,
    pub(crate) dstack_rev: &'a str,
    pub(crate) overlay_commit: &'a str,
    pub(crate) dstack_version: &'a str,
    pub(crate) flavors: &'a [Flavor],
}

/// Directories that one build uses.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BuildDirs<'a> {
    /// Scratch directory for the isolated clones of the reproducibility check.
    pub(crate) work_dir: &'a Path,
    /// Patched dstack checkout with the committed Outbe overlay.
    pub(crate) dstack_src: &'a Path,
    /// Final output directory.
    pub(crate) output_dir: &'a Path,
}

pub(crate) fn build_repro_command(
    script: &Path,
    output_dir: &Path,
    settings: &ReproBuildSettings<'_>,
) -> Command {
    let flavors_env = settings
        .flavors
        .iter()
        .map(|flavor| flavor.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut cmd = Command::new("nice");
    cmd.args(["-n", "19", "ionice", "-c3"])
        .arg(script)
        .env("FLAVORS", flavors_env)
        .env("JOBS", settings.jobs.to_string())
        .env("DSTACK_MKOSI_IMAGE", settings.mkosi_image);
    if let Some(cpuset) = settings.cpuset_cpus {
        cmd.env("DOCKER_CPUSET_CPUS", cpuset);
    }
    cmd.arg("-o").arg(output_dir);
    cmd
}

pub(crate) fn run_repro_build(
    dstack_src: &Path,
    output_dir: &Path,
    settings: &ReproBuildSettings<'_>,
) -> Result<()> {
    let script = dstack_src.join("os/mkosi/repro-build/repro-build.sh");
    ensure!(
        script.is_file(),
        "dstack repro-build script is missing: {}",
        script.display()
    );
    let mut cmd = build_repro_command(&script, output_dir, settings);
    let status = cmd.status().wrap_err("run dstack repro-build.sh")?;
    ensure!(
        status.success(),
        "dstack repro-build.sh failed with {status}"
    );
    Ok(())
}

/// Run either a single normal build into `output_dir`, or two isolated builds
/// (`a` and `b`) from separate `--no-hardlinks` clones at `overlay_commit` into
/// `output_dir/a` and `output_dir/b`, compare every flavor artifact byte for
/// byte via SHA-256, pack deterministic `.tar.zst` distribution archives in BOTH
/// runs and verify identical `archive_sha256`, then promote `output_dir/a/out`
/// and each verified archive into `output_dir`.
pub(crate) fn execute_repro_builds<F>(
    dirs: &BuildDirs<'_>,
    identity: &BuildIdentity<'_>,
    check: bool,
    mut run_build: F,
) -> Result<BTreeMap<String, FlavorArtifact>>
where
    F: FnMut(&Path, &Path) -> Result<()>,
{
    if !check {
        run_build(dirs.dstack_src, dirs.output_dir)?;
        return collect_and_pack_flavors(dirs.output_dir, identity);
    }

    let dstack_a = dirs.work_dir.join("dstack-a");
    let dstack_b = dirs.work_dir.join("dstack-b");
    clone_overlay_for_run(dirs.dstack_src, &dstack_a, identity.overlay_commit)?;
    clone_overlay_for_run(dirs.dstack_src, &dstack_b, identity.overlay_commit)?;

    let run_a_dir = dirs.output_dir.join("a");
    let run_b_dir = dirs.output_dir.join("b");
    fs::create_dir_all(&run_a_dir)?;
    fs::create_dir_all(&run_b_dir)?;

    run_build(&dstack_a, &run_a_dir)?;
    run_build(&dstack_b, &run_b_dir)?;

    compare_flavor_runs(
        &run_a_dir,
        &run_b_dir,
        identity.flavors,
        identity.dstack_version,
    )?;

    // Pack the distribution archive in BOTH isolated runs and require identical archive SHA-256.
    let artifacts_a = collect_and_pack_flavors(&run_a_dir, identity)?;
    let artifacts_b = collect_and_pack_flavors(&run_b_dir, identity)?;
    compare_packed_archives(&artifacts_a, &artifacts_b)?;

    promote_run_outputs(&run_a_dir, dirs.output_dir, &artifacts_a)?;
    Ok(artifacts_a)
}

/// Move the verified `out` tree and each packed archive of one run into `output_dir`.
fn promote_run_outputs(
    run_dir: &Path,
    output_dir: &Path,
    artifacts: &BTreeMap<String, FlavorArtifact>,
) -> Result<()> {
    fs::rename(run_dir.join("out"), output_dir.join("out"))
        .wrap_err("promote verified run a outputs to output/out")?;
    for artifact in artifacts.values() {
        if let Some(archive_name) = &artifact.archive {
            fs::rename(run_dir.join(archive_name), output_dir.join(archive_name))
                .wrap_err_with(|| format!("promote verified archive {archive_name}"))?;
        }
    }
    Ok(())
}

pub(crate) fn compare_packed_archives(
    artifacts_a: &BTreeMap<String, FlavorArtifact>,
    artifacts_b: &BTreeMap<String, FlavorArtifact>,
) -> Result<()> {
    let mut failures = Vec::new();
    for (flavor, art_a) in artifacts_a {
        let Some(art_b) = artifacts_b.get(flavor) else {
            failures.push(format!("flavor '{flavor}' missing from run b"));
            continue;
        };
        if art_a.archive != art_b.archive || art_a.archive_sha256 != art_b.archive_sha256 {
            failures.push(format!(
                "flavor '{flavor}' archive SHA-256 mismatch (run a={:?}, run b={:?})",
                art_a.archive_sha256, art_b.archive_sha256
            ));
        }
    }
    if !failures.is_empty() {
        bail!(
            "archive reproducibility check failed: {}",
            failures.join("; ")
        );
    }
    Ok(())
}

fn clone_overlay_for_run(dstack_src: &Path, dst: &Path, expected_commit: &str) -> Result<()> {
    let src_str = dstack_src
        .to_str()
        .ok_or_else(|| eyre::eyre!("non-UTF-8 dstack path: {}", dstack_src.display()))?;
    run_git(&["clone", "--quiet", "--no-hardlinks", src_str], Some(dst))?;
    run_git_in(dst, &["checkout", "--quiet", expected_commit])?;
    let actual = capture_git_in(dst, &["rev-parse", "HEAD"])?;
    ensure!(
        actual == expected_commit,
        "isolated clone HEAD {actual} does not match expected overlay commit {expected_commit}"
    );
    Ok(())
}

fn compare_flavor_runs(
    run_a_dir: &Path,
    run_b_dir: &Path,
    flavors: &[Flavor],
    dstack_version: &str,
) -> Result<()> {
    let mut failures = Vec::new();
    for flavor in flavors {
        let rel = flavor_image_rel(flavor, dstack_version);
        let mismatched = mismatched_flavor_files(&run_a_dir.join(&rel), &run_b_dir.join(&rel))?;
        if !mismatched.is_empty() {
            failures.push(format!(
                "flavor '{}' mismatched files: {}",
                flavor.as_str(),
                mismatched.join(", ")
            ));
        }
    }

    if !failures.is_empty() {
        bail!("reproducibility check failed: {}", failures.join("; "));
    }
    Ok(())
}

/// List the files that differ between the two runs of one flavor image directory. A missing or
/// empty `digest.txt` always counts as a mismatch.
fn mismatched_flavor_files(dir_a: &Path, dir_b: &Path) -> Result<Vec<String>> {
    ensure!(
        dir_a.is_dir(),
        "run a is missing flavor image dir: {}",
        dir_a.display()
    );
    ensure!(
        dir_b.is_dir(),
        "run b is missing flavor image dir: {}",
        dir_b.display()
    );

    let mut mismatched = Vec::new();
    let digest_a = read_trimmed_digest(dir_a);
    let digest_b = read_trimmed_digest(dir_b);
    if digest_a.is_none() || digest_a != digest_b {
        mismatched.push("digest.txt".to_owned());
    }

    let files_a = hash_directory_tree(dir_a)?;
    let files_b = hash_directory_tree(dir_b)?;
    let all_paths: BTreeSet<&str> = files_a
        .keys()
        .chain(files_b.keys())
        .map(String::as_str)
        .collect();
    for path in all_paths {
        if files_a.get(path) != files_b.get(path) && !mismatched.iter().any(|m| m == path) {
            mismatched.push(path.to_owned());
        }
    }
    Ok(mismatched)
}

/// Read the trimmed `digest.txt` of a flavor image directory, or `None` when it is missing or
/// empty.
fn read_trimmed_digest(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("digest.txt"))
        .map(|s| s.trim().to_owned())
        .ok()
        .filter(|s| !s.is_empty())
}

pub(crate) fn hash_directory_tree(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    collect_tree_hashes(root, root, &mut hashes)?;
    Ok(hashes)
}

fn collect_tree_hashes(
    root: &Path,
    dir: &Path,
    hashes: &mut BTreeMap<String, String>,
) -> Result<()> {
    for entry in fs::read_dir(dir).wrap_err_with(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let rel = path
            .strip_prefix(root)
            .wrap_err("strip flavor directory prefix")?
            .to_string_lossy()
            .replace('\\', "/");
        if meta.file_type().is_dir() {
            collect_tree_hashes(root, &path, hashes)?;
        } else if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?;
            hashes.insert(rel, format!("symlink:{}", target.to_string_lossy()));
        } else {
            hashes.insert(rel, sha256_file(&path)?);
        }
    }
    Ok(())
}

pub(crate) fn flavor_image_rel(flavor: &Flavor, dstack_version: &str) -> String {
    format!(
        "out/{}/{}-{dstack_version}",
        flavor.as_str(),
        flavor.image_prefix()
    )
}

pub(crate) fn read_dstack_version(dstack_src: &Path) -> Result<String> {
    let raw = fs::read_to_string(dstack_src.join("os/mkosi/versions.env"))
        .wrap_err("read dstack os/mkosi/versions.env")?;
    for line in raw.lines() {
        if let Some(value) = line.trim().strip_prefix("DSTACK_VERSION=") {
            return Ok(value.trim_matches('"').to_owned());
        }
    }
    bail!("DSTACK_VERSION not found in os/mkosi/versions.env");
}

fn collect_and_pack_flavors(
    base_dir: &Path,
    identity: &BuildIdentity<'_>,
) -> Result<BTreeMap<String, FlavorArtifact>> {
    let mut map = BTreeMap::new();
    for flavor in identity.flavors {
        let rel = flavor_image_rel(flavor, identity.dstack_version);
        let dir = base_dir.join(&rel);
        let os_image_hash = archive::read_digest_txt(&dir)?;
        let archive_name = archive::archive_filename(*flavor, &os_image_hash)?;
        let archive_path = base_dir.join(&archive_name);
        let (archive_manifest, archive_sha256) = archive::pack_flavor_archive(
            &dir,
            &archive::ArchiveBuildMetadata {
                flavor: *flavor,
                source_commit: identity.source_commit,
                dstack_rev: identity.dstack_rev,
                overlay_commit: identity.overlay_commit,
            },
            &archive_path,
        )?;
        map.insert(
            flavor.as_str().to_owned(),
            FlavorArtifact {
                os_image_hash,
                image_dir: rel,
                archive: Some(archive_name),
                archive_sha256: Some(archive_sha256),
                files: archive_manifest.files,
            },
        );
    }
    Ok(map)
}
#[cfg(test)]
mod tests;
