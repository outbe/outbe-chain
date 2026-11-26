//! Reproducible Outbe VM guest image builder on top of a pinned dstack revision.

pub mod archive;
mod overlay;
mod repro;

use std::{collections::BTreeMap, fs, io, path::Path, process::Command};

use eyre::{bail, ensure, Context, Result};
use outbe_software_bundle::{sha256_file, verify_and_extract};
use serde::{Deserialize, Serialize};

pub use overlay::stage_overlay;
use repro::{
    execute_repro_builds, read_dstack_version, run_repro_build, BuildDirs, BuildIdentity,
    ReproBuildSettings,
};

pub const DEFAULT_DSTACK_REPO: &str = "https://github.com/Dstack-TEE/dstack.git";
pub const DEFAULT_TOOLCHAIN_IMAGE: &str = "outbe-project-toolchain-runtime";
pub const DEFAULT_MKOSI_IMAGE: &str = "dstack-mkosi-build";
pub const DSTACK_COMMIT: &str = "c88f60e781a62d542e0843b333f18a48d1458b88";
pub const MANIFEST_NAME: &str = "vm-image-manifest.json";
const MANIFEST_VERSION: u32 = 1;

/// Fixed UID and GID for the `outbe` system user baked into the guest image.
pub const OUTBE_UID: u32 = 900;
pub const OUTBE_GID: u32 = 900;

/// Binaries copied from the verified software bundle into `/usr/local/bin`.
/// The CRS is intentionally omitted from the image and stays on the data disk.
/// `outbe-up` and `outbe-net-up` are intentionally excluded (R-4 / S8); guest units invoke
/// `/usr/local/bin/outbe-vm-agent` instead.
pub const VM_BINARIES: &[&str] = &[
    "outbe-chain",
    "outbe-radicle",
    "outbe-feeder",
    "outbe-hyperlane-validator",
    "outbe-vm-agent",
    "outbe-cli",
    "outbe-keygen",
];

/// Pinned dstack patches applied in order from `deploy/outbe-vm/patches/`.
pub const PINNED_PATCHES: &[PinnedFile] = &[
    PinnedFile {
        name: "0001-unencrypted-ext4-data-disk.patch",
        sha256: "da37103205ac55ea2c1d52d1c6f490cfd63fb043ba2f01c5d3d0c27f998dee82",
    },
    PinnedFile {
        name: "0002-sev-crate-8-report-version-5.patch",
        sha256: "bdfe6cc672621696f4c20ceddfd51d0528d64abf5172d6ab775b3d6b5a57ae9f",
    },
    PinnedFile {
        name: "0003-squashfs-root-mode-0755.patch",
        sha256: "d97dc640ed27e7b9263c481f523f5aafb470d00efac4bd73d4f2137fc155738f",
    },
    PinnedFile {
        name: "0004-repro-build-load-and-env.patch",
        sha256: "eaaa79522b654c8d4bc262baf369700360147c757b81edf135e2fa803409c22a",
    },
];

/// Pinned LLVM 18 runtime libraries missing from the Debian trixie guest image.
/// Extracted from the pinned Ubuntu 24.04 `Dockerfile.project-toolchain` builder.
pub const PINNED_RUNTIME_LIBS: &[PinnedFile] = &[
    PinnedFile {
        name: "libc++.so.1",
        sha256: "465b54c395f9a227554e653cdf0e2742686f891eb278e8d0b630d0229084c0df",
    },
    PinnedFile {
        name: "libc++abi.so.1",
        sha256: "40cb6e2e548b61e53d4e3fb07c537dbc4f23900f465ae584f141eda9c92b1585",
    },
    PinnedFile {
        name: "libunwind.so.1",
        sha256: "b9b34fb09f6c86778dd8a73f11715d09bb89efafbb4331bc76905b75cf1fb892",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedFile {
    pub name: &'static str,
    pub sha256: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Flavor {
    Dev,
    Prod,
}

impl Flavor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Prod => "prod",
        }
    }

    const fn image_prefix(self) -> &'static str {
        match self {
            Self::Dev => "dstack-dev",
            Self::Prod => "dstack",
        }
    }
}

pub struct VmImageOptions<'a> {
    pub repo_root: &'a Path,
    pub software_bundle: &'a Path,
    pub output_dir: &'a Path,
    pub flavors: &'a [Flavor],
    pub dstack_repo: &'a str,
    pub runtime_libs_dir: Option<&'a Path>,
    pub toolchain_image: &'a str,
    pub mkosi_image: &'a str,
    pub cpuset_cpus: Option<&'a str>,
    pub jobs: usize,
    pub check: bool,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VmImageManifest {
    pub version: u32,
    pub source_commit: String,
    pub software_bundle_sha256: String,
    pub dstack_rev: String,
    pub overlay_commit: String,
    pub reproducible: bool,
    pub patches: Vec<ManifestDigest>,
    pub runtime_libs: Vec<ManifestDigest>,
    pub flavors: BTreeMap<String, FlavorArtifact>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManifestDigest {
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FlavorArtifact {
    pub os_image_hash: String,
    pub image_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<archive::ImageFileEntry>,
}

/// Parse a comma-separated list of dstack flavors (`dev`, `prod`).
pub fn parse_flavors(raw: &str) -> Result<Vec<Flavor>> {
    let mut flavors = Vec::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let flavor = match item {
            "dev" => Flavor::Dev,
            "prod" => Flavor::Prod,
            other => {
                bail!("unsupported VM image flavor '{other}' (expected dev or prod)");
            }
        };
        if !flavors.contains(&flavor) {
            flavors.push(flavor);
        }
    }
    ensure!(
        !flavors.is_empty(),
        "at least one VM image flavor is required"
    );
    Ok(flavors)
}

/// Build the dstack guest image(s) from a verified software bundle and write `vm-image-manifest.json`.
pub fn build(options: &VmImageOptions<'_>) -> Result<VmImageManifest> {
    ensure!(!options.flavors.is_empty(), "flavors must not be empty");
    ensure!(options.jobs > 0, "jobs must be positive");
    match fs::symlink_metadata(options.output_dir) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).wrap_err_with(|| {
                format!(
                    "failed to stat output directory {}",
                    options.output_dir.display()
                )
            });
        }
        Ok(_) => {
            return Err(eyre::eyre!(
                "output directory already exists: {}",
                options.output_dir.display()
            ));
        }
    }

    require_clean_checkout(options.repo_root)?;
    let source_commit = git_head_commit(options.repo_root)?;
    let patches = verify_pinned_patches(options.repo_root)?;

    let parent = options
        .output_dir
        .parent()
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let work = tempfile::Builder::new()
        .prefix(".outbe-vm-image-")
        .tempdir_in(parent)?;

    let extracted_libs_dir;
    let runtime_libs_dir = match options.runtime_libs_dir {
        Some(dir) => dir,
        None => {
            extracted_libs_dir = work.path().join("runtime-libs");
            extract_runtime_libs_from_toolchain(
                options.repo_root,
                &extracted_libs_dir,
                options.toolchain_image,
            )?;
            &extracted_libs_dir
        }
    };
    let runtime_libs = verify_pinned_runtime_libs(runtime_libs_dir)?;

    let bundle_sha256 = sha256_file(options.software_bundle)?;
    let software_root = work.path().join("software");
    fs::create_dir(&software_root)?;
    let software_manifest = verify_and_extract(options.software_bundle, &software_root)
        .wrap_err("software bundle verification failed")?;
    ensure!(
        software_manifest.commit == source_commit,
        "software bundle commit {} does not match current repository HEAD {source_commit}",
        software_manifest.commit
    );

    let dstack_src = work.path().join("dstack-outbe");
    clone_and_patch_dstack(options.dstack_repo, &dstack_src, options.repo_root)?;

    stage_overlay(&dstack_src, &software_root, runtime_libs_dir)?;
    let overlay_commit = commit_overlay(&dstack_src)?;

    fs::create_dir_all(options.output_dir)?;
    let output_abs = fs::canonicalize(options.output_dir)?;
    let dstack_version = read_dstack_version(&dstack_src)?;
    let settings = ReproBuildSettings::from_options(options);
    let flavors = execute_repro_builds(
        &BuildDirs {
            work_dir: work.path(),
            dstack_src: &dstack_src,
            output_dir: &output_abs,
        },
        &BuildIdentity {
            source_commit: &source_commit,
            dstack_rev: DSTACK_COMMIT,
            overlay_commit: &overlay_commit,
            dstack_version: &dstack_version,
            flavors: options.flavors,
        },
        options.check,
        |run_src, run_out| run_repro_build(run_src, run_out, &settings),
    )?;

    let manifest = VmImageManifest {
        version: MANIFEST_VERSION,
        source_commit,
        software_bundle_sha256: bundle_sha256,
        dstack_rev: DSTACK_COMMIT.to_owned(),
        overlay_commit,
        reproducible: options.check,
        patches,
        runtime_libs,
        flavors,
    };
    write_manifest(&output_abs, &manifest)?;
    Ok(manifest)
}

fn write_manifest(output_dir: &Path, manifest: &VmImageManifest) -> Result<()> {
    fs::write(
        output_dir.join(MANIFEST_NAME),
        format!("{}\n", serde_json::to_string_pretty(manifest)?),
    )?;
    Ok(())
}
pub fn verify_pinned_patches(repo_root: &Path) -> Result<Vec<ManifestDigest>> {
    let dir = repo_root.join("deploy/outbe-vm/patches");
    verify_pinned_set(&dir, PINNED_PATCHES, "dstack patch")
}

pub fn verify_pinned_runtime_libs(libs_dir: &Path) -> Result<Vec<ManifestDigest>> {
    verify_pinned_set(libs_dir, PINNED_RUNTIME_LIBS, "runtime library")
}

const TOOLCHAIN_LIB_DIR: &str = "/usr/lib/x86_64-linux-gnu";

fn build_toolchain_docker_args(toolchain_image: &str) -> Vec<&str> {
    vec![
        "build",
        "--load",
        "--platform",
        "linux/amd64",
        "--file",
        "Dockerfile.project-toolchain",
        "--target",
        "toolchain",
        "--tag",
        toolchain_image,
        ".",
    ]
}

/// Build or reuse the `toolchain` target of `Dockerfile.project-toolchain` and
/// copy the three pinned LLVM 18 runtime libraries out of it.
fn extract_runtime_libs_from_toolchain(
    repo_root: &Path,
    dest: &Path,
    toolchain_image: &str,
) -> Result<()> {
    fs::create_dir_all(dest)?;
    let build_status = Command::new("docker")
        .current_dir(repo_root)
        .args(build_toolchain_docker_args(toolchain_image))
        .status()
        .wrap_err("run docker build for Dockerfile.project-toolchain target toolchain")?;
    ensure!(
        build_status.success(),
        "docker build for toolchain target failed with {build_status}"
    );

    let create_out = Command::new("docker")
        .args([
            "create",
            "--platform",
            "linux/amd64",
            toolchain_image,
            "true",
        ])
        .output()
        .wrap_err("run docker create for toolchain runtime library extraction")?;
    ensure!(
        create_out.status.success(),
        "docker create failed: {}",
        String::from_utf8_lossy(&create_out.stderr).trim()
    );
    let container_id = String::from_utf8(create_out.stdout)?.trim().to_owned();
    ensure!(!container_id.is_empty(), "docker create returned empty id");

    let copy_result = (|| -> Result<()> {
        for lib in PINNED_RUNTIME_LIBS {
            let src = format!("{container_id}:{TOOLCHAIN_LIB_DIR}/{}", lib.name);
            let dst = dest.join(lib.name);
            let status = Command::new("docker")
                .args(["cp", "-L", &src])
                .arg(&dst)
                .status()
                .wrap_err_with(|| format!("copy {} from toolchain container", lib.name))?;
            ensure!(
                status.success(),
                "docker cp -L failed for {} with {status}",
                lib.name
            );
        }
        Ok(())
    })();

    let _ = Command::new("docker")
        .args(["rm", "-f", &container_id])
        .output();
    copy_result
}

fn verify_pinned_set(
    dir: &Path,
    pinned: &[PinnedFile],
    label: &str,
) -> Result<Vec<ManifestDigest>> {
    let mut verified = Vec::with_capacity(pinned.len());
    for item in pinned {
        let path = dir.join(item.name);
        ensure!(path.is_file(), "missing pinned {label}: {}", path.display());
        let actual = sha256_file(&path)?;
        ensure!(
            actual == item.sha256,
            "pinned {label} {} digest mismatch: expected {}, got {actual}",
            item.name,
            item.sha256
        );
        verified.push(ManifestDigest {
            name: item.name.to_owned(),
            sha256: actual,
        });
    }
    Ok(verified)
}

fn clone_and_patch_dstack(dstack_repo: &str, dst: &Path, repo_root: &Path) -> Result<()> {
    run_git(
        &["clone", "--quiet", "--no-hardlinks", dstack_repo],
        Some(dst),
    )?;
    run_git_in(dst, &["checkout", "--quiet", DSTACK_COMMIT])?;
    let patch_dir = repo_root.join("deploy/outbe-vm/patches");
    for patch in PINNED_PATCHES {
        let path = patch_dir.join(patch.name);
        let path_str = path
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 patch path: {}", path.display()))?;
        run_git_in(dst, &["apply", path_str])?;
    }
    Ok(())
}

fn commit_overlay(dstack_src: &Path) -> Result<String> {
    let commit_date = capture_git_in(dstack_src, &["log", "-1", "--format=%cI", DSTACK_COMMIT])?;
    run_git_in(dstack_src, &["add", "-A"])?;
    let status = Command::new("git")
        .current_dir(dstack_src)
        .env("GIT_AUTHOR_DATE", &commit_date)
        .env("GIT_COMMITTER_DATE", &commit_date)
        .args([
            "-c",
            "user.name=outbe-image-build",
            "-c",
            "user.email=outbe-image-build@localhost",
            "commit",
            "--quiet",
            "-m",
            "outbe validator image overlay",
        ])
        .status()
        .wrap_err("run git commit on dstack overlay")?;
    ensure!(status.success(), "git commit on dstack overlay failed");
    capture_git_in(dstack_src, &["rev-parse", "HEAD"])
}

fn require_clean_checkout(repo_root: &Path) -> Result<()> {
    let status = capture_git_in(repo_root, &["status", "--porcelain"])?;
    ensure!(
        status.is_empty(),
        "VM image build requires a clean git checkout; commit or stash local changes first"
    );
    Ok(())
}

fn git_head_commit(repo_root: &Path) -> Result<String> {
    capture_git_in(repo_root, &["rev-parse", "HEAD"])
}

fn run_git(args: &[&str], dest: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(dst) = dest {
        cmd.arg(dst);
    }
    let status = cmd.status().wrap_err("run git")?;
    ensure!(status.success(), "git {:?} failed with {status}", args);
    Ok(())
}

fn run_git_in(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .wrap_err("run git")?;
    ensure!(status.success(), "git {:?} failed with {status}", args);
    Ok(())
}

fn capture_git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .wrap_err("run git")?;
    ensure!(output.status.success(), "git {:?} failed", args);
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod tests;
