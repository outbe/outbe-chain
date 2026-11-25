//! Reproducible Outbe VM guest image builder on top of a pinned dstack revision.

pub mod archive;

use std::{
    collections::BTreeMap, fs, io, os::unix::fs::PermissionsExt, path::Path, process::Command,
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_systemd::vm_units;
use outbe_software_bundle::{sha256_file, verify_and_extract};
use serde::{Deserialize, Serialize};

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
pub const VM_BINARIES: &[&str] = &[
    "outbe-chain",
    "outbe-radicle",
    "outbe-feeder",
    "outbe-hyperlane-validator",
    "outbe-up",
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
    let flavors = execute_repro_builds(
        work.path(),
        &dstack_src,
        &source_commit,
        DSTACK_COMMIT,
        &overlay_commit,
        &output_abs,
        options.flavors,
        &dstack_version,
        options.check,
        |run_src, run_out| {
            run_repro_build(
                run_src,
                run_out,
                options.flavors,
                options.jobs,
                options.mkosi_image,
                options.cpuset_cpus,
            )
        },
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

/// Stage the Outbe rootfs overlay into `dstack_root/os/mkosi/mkosi.skeleton`
/// (shared by `dev` and `prod`) and `mkosi.profiles/dev/mkosi.skeleton` (`dev` only).
pub fn stage_overlay(
    dstack_root: &Path,
    software_root: &Path,
    runtime_libs_dir: &Path,
) -> Result<()> {
    let common = dstack_root.join("os/mkosi/mkosi.skeleton");
    let dev_only = dstack_root.join("os/mkosi/mkosi.profiles/dev/mkosi.skeleton");
    fs::create_dir_all(&common)?;
    fs::create_dir_all(&dev_only)?;

    // 1. Binaries from verified software bundle -> /usr/local/bin (no CRS in image).
    let bin_dst = common.join("usr/local/bin");
    fs::create_dir_all(&bin_dst)?;
    for binary in VM_BINARIES {
        let src = software_root.join("bin").join(binary);
        ensure!(
            src.is_file(),
            "software bundle is missing required VM binary: {}",
            src.display()
        );
        let dst = bin_dst.join(binary);
        fs::copy(&src, &dst)?;
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))?;
    }

    // 2. Pinned LLVM 18 runtime libraries -> /usr/local/lib/outbe/lib + ld.so.conf.d.
    let lib_dst = common.join("usr/local/lib/outbe/lib");
    fs::create_dir_all(&lib_dst)?;
    for pinned in PINNED_RUNTIME_LIBS {
        let src = runtime_libs_dir.join(pinned.name);
        let dst = lib_dst.join(pinned.name);
        fs::copy(&src, &dst)?;
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o644))?;
    }
    write_overlay_file(
        &common.join("etc/ld.so.conf.d/outbe.conf"),
        "/usr/local/lib/outbe/lib\n",
        0o644,
    )?;

    // 3. System account `outbe` with fixed UID/GID and home `/opt/outbe-chain`.
    write_overlay_file(
        &common.join("usr/lib/sysusers.d/outbe.conf"),
        &render_sysusers_conf(),
        0o644,
    )?;

    // 4. Mountpoint `/opt/outbe-chain` in the read-only rootfs.
    write_overlay_file(&common.join("opt/outbe-chain/.keep"), "", 0o644)?;

    // 5. Systemd units from `outbe_operator_systemd::vm_units()` + data bind-mount unit.
    let systemd_dir = common.join("usr/lib/systemd/system");
    fs::create_dir_all(&systemd_dir)?;
    for unit in vm_units() {
        write_overlay_file(&systemd_dir.join(unit.name), &unit.content, 0o644)?;
    }
    write_overlay_file(
        &systemd_dir.join("outbe-data-mount.service"),
        &render_data_mount_unit(),
        0o644,
    )?;

    // 6. Preset with `dstack` in its filename so `mkosi.postinst` preserves it.
    write_overlay_file(
        &common.join("usr/lib/systemd/system-preset/85-dstack-outbe.preset"),
        "enable outbe-data-mount.service\n",
        0o644,
    )?;

    // 7. Dev-only SSH configuration: read authorized_keys from the unmeasured data disk.
    //    Lives under `mkosi.profiles/dev/mkosi.skeleton`, so `prod` never receives it.
    //    Named `00-outbe-dev.conf` so OpenSSH first-match-wins overrides dstack's `10-*`/`50-*`.
    write_overlay_file(
        &dev_only.join("etc/ssh/sshd_config.d/00-outbe-dev.conf"),
        &render_dev_sshd_conf(),
        0o644,
    )?;
    write_overlay_file(
        &dev_only.join("usr/lib/systemd/system/ssh.service.d/20-outbe-dev.conf"),
        "[Unit]\nAfter=outbe-data-mount.service\nWants=outbe-data-mount.service\n",
        0o644,
    )?;
    write_overlay_file(
        &dev_only.join("usr/lib/systemd/system-preset/85-dstack-outbe-dev.preset"),
        "enable ssh.service\n",
        0o644,
    )?;

    Ok(())
}

fn render_sysusers_conf() -> String {
    format!(
        "# Fixed UID/GID for the Outbe validator service account.\n\
         g outbe {OUTBE_GID} -\n\
         u outbe {OUTBE_UID}:{OUTBE_GID} \"Outbe validator\" /opt/outbe-chain /usr/sbin/nologin\n"
    )
}

fn render_data_mount_unit() -> String {
    concat!(
        "[Unit]\n",
        "Description=Bind-mount Outbe persistent data directory with noexec,nosuid,nodev\n",
        "Requires=dstack-prepare.service\n",
        "After=dstack-prepare.service\n",
        "Before=outbe-vm-boot.service\n",
        "PartOf=outbe-validator.target\n",
        "\n",
        "[Service]\n",
        "Type=oneshot\n",
        "RemainAfterExit=yes\n",
        "ExecStart=/usr/bin/mount --bind /dstack/persistent/outbe-chain /opt/outbe-chain\n",
        "ExecStart=/usr/bin/mount -o remount,bind,noexec,nosuid,nodev /opt/outbe-chain\n",
        "ExecStop=/usr/bin/umount /opt/outbe-chain\n",
        "StandardOutput=journal+console\n",
        "StandardError=journal+console\n",
        "\n",
        "[Install]\n",
        "WantedBy=outbe-validator.target\n",
    )
    .to_owned()
}

fn render_dev_sshd_conf() -> String {
    concat!(
        "# Outbe dev VM SSH access using keys on the unmeasured data disk.\n",
        "PubkeyAuthentication yes\n",
        "PermitRootLogin prohibit-password\n",
        "AuthorizedKeysFile /opt/outbe-chain/dev/authorized_keys\n",
    )
    .to_owned()
}

fn write_overlay_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
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

fn build_repro_command(
    script: &Path,
    output_dir: &Path,
    flavors: &[Flavor],
    jobs: usize,
    mkosi_image: &str,
    cpuset_cpus: Option<&str>,
) -> Command {
    let flavors_env = flavors
        .iter()
        .map(|flavor| flavor.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut cmd = Command::new("nice");
    cmd.args(["-n", "19", "ionice", "-c3"])
        .arg(script)
        .env("FLAVORS", flavors_env)
        .env("JOBS", jobs.to_string())
        .env("DSTACK_MKOSI_IMAGE", mkosi_image);
    if let Some(cpuset) = cpuset_cpus {
        cmd.env("DOCKER_CPUSET_CPUS", cpuset);
    }
    cmd.arg("-o").arg(output_dir);
    cmd
}

fn run_repro_build(
    dstack_src: &Path,
    output_dir: &Path,
    flavors: &[Flavor],
    jobs: usize,
    mkosi_image: &str,
    cpuset_cpus: Option<&str>,
) -> Result<()> {
    let script = dstack_src.join("os/mkosi/repro-build/repro-build.sh");
    ensure!(
        script.is_file(),
        "dstack repro-build script is missing: {}",
        script.display()
    );
    let mut cmd = build_repro_command(&script, output_dir, flavors, jobs, mkosi_image, cpuset_cpus);
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
#[allow(clippy::too_many_arguments)]
fn execute_repro_builds<F>(
    work_dir: &Path,
    dstack_src: &Path,
    source_commit: &str,
    dstack_rev: &str,
    overlay_commit: &str,
    output_dir: &Path,
    flavors: &[Flavor],
    dstack_version: &str,
    check: bool,
    mut run_build: F,
) -> Result<BTreeMap<String, FlavorArtifact>>
where
    F: FnMut(&Path, &Path) -> Result<()>,
{
    if !check {
        run_build(dstack_src, output_dir)?;
        return collect_and_pack_flavors(
            output_dir,
            flavors,
            dstack_version,
            source_commit,
            dstack_rev,
            overlay_commit,
        );
    }

    let dstack_a = work_dir.join("dstack-a");
    let dstack_b = work_dir.join("dstack-b");
    clone_overlay_for_run(dstack_src, &dstack_a, overlay_commit)?;
    clone_overlay_for_run(dstack_src, &dstack_b, overlay_commit)?;

    let run_a_dir = output_dir.join("a");
    let run_b_dir = output_dir.join("b");
    fs::create_dir_all(&run_a_dir)?;
    fs::create_dir_all(&run_b_dir)?;

    run_build(&dstack_a, &run_a_dir)?;
    run_build(&dstack_b, &run_b_dir)?;

    compare_flavor_runs(&run_a_dir, &run_b_dir, flavors, dstack_version)?;

    // Pack the distribution archive in BOTH isolated runs and require identical archive SHA-256.
    let artifacts_a = collect_and_pack_flavors(
        &run_a_dir,
        flavors,
        dstack_version,
        source_commit,
        dstack_rev,
        overlay_commit,
    )?;
    let artifacts_b = collect_and_pack_flavors(
        &run_b_dir,
        flavors,
        dstack_version,
        source_commit,
        dstack_rev,
        overlay_commit,
    )?;
    compare_packed_archives(&artifacts_a, &artifacts_b)?;

    fs::rename(run_a_dir.join("out"), output_dir.join("out"))
        .wrap_err("promote verified run a outputs to output/out")?;
    for artifact in artifacts_a.values() {
        if let Some(archive_name) = &artifact.archive {
            fs::rename(run_a_dir.join(archive_name), output_dir.join(archive_name))
                .wrap_err_with(|| format!("promote verified archive {archive_name}"))?;
        }
    }
    Ok(artifacts_a)
}

fn compare_packed_archives(
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
        let dir_a = run_a_dir.join(&rel);
        let dir_b = run_b_dir.join(&rel);
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
        let digest_a = fs::read_to_string(dir_a.join("digest.txt"))
            .map(|s| s.trim().to_owned())
            .ok()
            .filter(|s| !s.is_empty());
        let digest_b = fs::read_to_string(dir_b.join("digest.txt"))
            .map(|s| s.trim().to_owned())
            .ok()
            .filter(|s| !s.is_empty());
        if digest_a.is_none() || digest_a != digest_b {
            mismatched.push("digest.txt".to_owned());
        }

        let files_a = hash_directory_tree(&dir_a)?;
        let files_b = hash_directory_tree(&dir_b)?;
        let all_paths: std::collections::BTreeSet<&str> = files_a
            .keys()
            .chain(files_b.keys())
            .map(String::as_str)
            .collect();
        for path in all_paths {
            if files_a.get(path) != files_b.get(path) && !mismatched.iter().any(|m| m == path) {
                mismatched.push(path.to_owned());
            }
        }

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

fn hash_directory_tree(root: &Path) -> Result<BTreeMap<String, String>> {
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

fn flavor_image_rel(flavor: &Flavor, dstack_version: &str) -> String {
    format!(
        "out/{}/{}-{dstack_version}",
        flavor.as_str(),
        flavor.image_prefix()
    )
}

fn read_dstack_version(dstack_src: &Path) -> Result<String> {
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
    flavors: &[Flavor],
    dstack_version: &str,
    source_commit: &str,
    dstack_rev: &str,
    overlay_commit: &str,
) -> Result<BTreeMap<String, FlavorArtifact>> {
    let mut map = BTreeMap::new();
    for flavor in flavors {
        let rel = flavor_image_rel(flavor, dstack_version);
        let dir = base_dir.join(&rel);
        let os_image_hash = archive::read_digest_txt(&dir)?;
        let archive_name = archive::archive_filename(*flavor, &os_image_hash)?;
        let archive_path = base_dir.join(&archive_name);
        let (archive_manifest, archive_sha256) = archive::pack_flavor_archive(
            &dir,
            &archive::ArchiveBuildMetadata {
                flavor: *flavor,
                source_commit,
                dstack_rev,
                overlay_commit,
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
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn repository_root() -> Result<PathBuf> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .wrap_err("resolve repository root")?;
        ensure!(
            output.status.success(),
            "git rev-parse --show-toplevel failed"
        );
        let root = String::from_utf8(output.stdout)?;
        fs::canonicalize(root.trim()).wrap_err("canonicalize repository root")
    }

    #[test]
    fn pinned_dstack_rev_patches_and_app_compose_match_repository() {
        let root = repository_root().unwrap();
        assert_eq!(DSTACK_COMMIT.len(), 40);
        let patches = verify_pinned_patches(&root).unwrap();
        assert_eq!(patches.len(), 4);
        let patch_0004 = fs::read_to_string(
            root.join("deploy/outbe-vm/patches/0004-repro-build-load-and-env.patch"),
        )
        .unwrap();
        assert!(patch_0004.contains("--entrypoint /usr/bin/nice"));
        assert!(
            patch_0004.contains("-n 19 /usr/bin/ionice -c3 /usr/local/bin/dstack-mkosi-entrypoint")
        );

        let compose: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("deploy/outbe-vm/app-compose.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(compose["runner"], "bash");
        assert_eq!(compose["storage_fs"], "ext4");
        assert_eq!(
            compose["bash_script"],
            "systemctl start --no-block outbe-validator.target\n"
        );
    }

    #[test]
    fn parse_flavors_validates_and_deduplicates() {
        assert_eq!(
            parse_flavors("dev,prod,dev").unwrap(),
            vec![Flavor::Dev, Flavor::Prod]
        );
        assert!(parse_flavors("").is_err());
        assert!(parse_flavors("staging").is_err());
    }

    #[test]
    fn overlay_assembly_stages_binaries_units_libs_and_keeps_ssh_dev_only() {
        let temp = tempfile::tempdir().unwrap();
        let dstack_root = temp.path().join("dstack");
        let software_root = temp.path().join("software");
        let libs_dir = temp.path().join("libs");
        fs::create_dir_all(software_root.join("bin")).unwrap();
        fs::create_dir_all(software_root.join(".bb-crs")).unwrap();
        fs::create_dir_all(&libs_dir).unwrap();

        for binary in VM_BINARIES {
            let path = software_root.join("bin").join(binary);
            fs::write(&path, format!("elf-{binary}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // CRS exists in the software bundle, but must NOT be copied into the VM image.
        fs::write(software_root.join(".bb-crs/bn254_g1.dat"), b"crs").unwrap();

        for lib in PINNED_RUNTIME_LIBS {
            fs::write(libs_dir.join(lib.name), format!("so-{}", lib.name)).unwrap();
        }

        stage_overlay(&dstack_root, &software_root, &libs_dir).unwrap();

        let common = dstack_root.join("os/mkosi/mkosi.skeleton");
        let dev_only = dstack_root.join("os/mkosi/mkosi.profiles/dev/mkosi.skeleton");

        // All 7 VM binaries installed with 0755; CRS omitted.
        for binary in VM_BINARIES {
            let dst = common.join("usr/local/bin").join(binary);
            assert!(dst.is_file(), "missing binary: {binary}");
            assert_eq!(
                fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        assert!(!common.join("opt/outbe-chain/.bb-crs").exists());
        assert!(!common.join(".bb-crs").exists());

        // Runtime libs and ld.so.conf.d present.
        for lib in PINNED_RUNTIME_LIBS {
            assert!(common
                .join("usr/local/lib/outbe/lib")
                .join(lib.name)
                .is_file());
        }
        assert_eq!(
            fs::read_to_string(common.join("etc/ld.so.conf.d/outbe.conf")).unwrap(),
            "/usr/local/lib/outbe/lib\n"
        );

        // System user, mountpoint, preset with `dstack` in name, all VM units + data mount unit.
        let sysusers = fs::read_to_string(common.join("usr/lib/sysusers.d/outbe.conf")).unwrap();
        assert!(sysusers.contains("u outbe 900:900"));
        assert!(sysusers.contains("/opt/outbe-chain"));
        assert!(common.join("opt/outbe-chain").is_dir());
        assert!(common
            .join("usr/lib/systemd/system-preset/85-dstack-outbe.preset")
            .is_file());

        let systemd = common.join("usr/lib/systemd/system");
        for unit in vm_units() {
            assert!(
                systemd.join(&unit.name).is_file(),
                "missing unit {}",
                unit.name
            );
        }
        let mount_unit = fs::read_to_string(systemd.join("outbe-data-mount.service")).unwrap();
        assert!(mount_unit.contains("mount --bind /dstack/persistent/outbe-chain /opt/outbe-chain"));
        assert!(mount_unit.contains("remount,bind,noexec,nosuid,nodev /opt/outbe-chain"));

        // SSH configuration and preset are strictly in dev profile skeleton, never in common/prod.
        assert!(!common.join("etc/ssh").exists());
        assert!(!common.join("usr/lib/systemd/system/ssh.service.d").exists());
        assert!(!common
            .join("usr/lib/systemd/system-preset/85-dstack-outbe-dev.preset")
            .exists());
        let dev_sshd =
            fs::read_to_string(dev_only.join("etc/ssh/sshd_config.d/00-outbe-dev.conf")).unwrap();
        assert!(dev_sshd.contains("PubkeyAuthentication yes"));
        assert!(dev_sshd.contains("PermitRootLogin prohibit-password"));
        assert!(!dev_sshd.contains("StrictModes"));
        assert!(dev_sshd.contains("AuthorizedKeysFile /opt/outbe-chain/dev/authorized_keys"));
        let dev_ssh_dropin = fs::read_to_string(
            dev_only.join("usr/lib/systemd/system/ssh.service.d/20-outbe-dev.conf"),
        )
        .unwrap();
        assert!(dev_ssh_dropin.contains("After=outbe-data-mount.service"));
        assert!(dev_ssh_dropin.contains("Wants=outbe-data-mount.service"));
        let dev_ssh_preset = fs::read_to_string(
            dev_only.join("usr/lib/systemd/system-preset/85-dstack-outbe-dev.preset"),
        )
        .unwrap();
        assert_eq!(dev_ssh_preset, "enable ssh.service\n");
    }

    #[test]
    fn rejects_corrupted_software_bundle_and_unverified_runtime_libs() {
        let temp = tempfile::tempdir().unwrap();
        let bad_bundle = temp.path().join("bad.tar.zst");
        fs::write(&bad_bundle, b"not-a-valid-bundle").unwrap();
        let out = temp.path().join("extracted");
        fs::create_dir(&out).unwrap();
        assert!(verify_and_extract(&bad_bundle, &out).is_err());

        let bad_libs = temp.path().join("bad-libs");
        fs::create_dir(&bad_libs).unwrap();
        for lib in PINNED_RUNTIME_LIBS {
            fs::write(bad_libs.join(lib.name), b"tampered").unwrap();
        }
        let err = verify_pinned_runtime_libs(&bad_libs).unwrap_err();
        assert!(err.to_string().contains("digest mismatch"), "{err}");
    }

    #[test]
    fn docker_build_passes_load_and_repro_command_forwards_image_and_cpuset() {
        let args = build_toolchain_docker_args("outbe-toolchain:custom");
        assert!(args.contains(&"--load"));
        assert!(args
            .windows(2)
            .any(|w| w == ["--tag", "outbe-toolchain:custom"]));

        let cmd = build_repro_command(
            Path::new("/tmp/repro-build.sh"),
            Path::new("/tmp/out"),
            &[Flavor::Dev],
            4,
            "outbe-mkosi:custom",
            Some("0-3"),
        );
        let cmd_args: Vec<_> = cmd.get_args().filter_map(|a| a.to_str()).collect();
        assert!(!cmd_args.contains(&"-c"));
        let envs: BTreeMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?, v?.to_str()?)))
            .collect();
        assert_eq!(envs.get("FLAVORS"), Some(&"dev"));
        assert_eq!(envs.get("JOBS"), Some(&"4"));
        assert_eq!(envs.get("DSTACK_MKOSI_IMAGE"), Some(&"outbe-mkosi:custom"));
        assert_eq!(envs.get("DOCKER_CPUSET_CPUS"), Some(&"0-3"));
    }

    fn init_fake_overlay_repo(dir: &Path) -> String {
        fs::create_dir_all(dir).unwrap();
        run_git_in(dir, &["init", "--quiet"]).unwrap();
        fs::write(dir.join("marker.txt"), "overlay-v1\n").unwrap();
        run_git_in(dir, &["add", "-A"]).unwrap();
        let status = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@localhost",
                "commit",
                "--quiet",
                "-m",
                "init",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        capture_git_in(dir, &["rev-parse", "HEAD"]).unwrap()
    }

    fn write_fake_flavor_output(
        build_dir: &Path,
        flavor: Flavor,
        version: &str,
        digest: &str,
        rootfs: &[u8],
    ) {
        let dir = build_dir.join(flavor_image_rel(&flavor, version));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("digest.txt"), format!("{digest}\n")).unwrap();
        fs::write(dir.join("rootfs.img"), rootfs).unwrap();
        fs::write(dir.join("metadata.json"), "{\"rootfs\":\"rootfs.img\"}\n").unwrap();
    }

    #[test]
    fn repro_check_two_run_orchestration_passes_for_dev_and_prod_and_writes_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let work_dir = temp.path().join("work");
        let dstack_src = work_dir.join("dstack-outbe");
        let output_dir = temp.path().join("output");
        fs::create_dir_all(&output_dir).unwrap();
        let overlay_commit = init_fake_overlay_repo(&dstack_src);
        let dev_hash = "1".repeat(64);
        let prod_hash = "2".repeat(64);

        let mut seen_runs = Vec::new();
        let flavors = execute_repro_builds(
            &work_dir,
            &dstack_src,
            &"a".repeat(40),
            DSTACK_COMMIT,
            &overlay_commit,
            &output_dir,
            &[Flavor::Dev, Flavor::Prod],
            "0.6.3",
            true,
            |run_src, run_out| {
                assert_ne!(run_src, dstack_src);
                assert_eq!(
                    capture_git_in(run_src, &["rev-parse", "HEAD"]).unwrap(),
                    overlay_commit
                );
                seen_runs.push((run_src.to_path_buf(), run_out.to_path_buf()));
                write_fake_flavor_output(run_out, Flavor::Dev, "0.6.3", &dev_hash, b"dev-img");
                write_fake_flavor_output(run_out, Flavor::Prod, "0.6.3", &prod_hash, b"prod-img");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(seen_runs.len(), 2);
        assert_ne!(seen_runs[0].0, seen_runs[1].0);
        assert_eq!(seen_runs[0].1, output_dir.join("a"));
        assert_eq!(seen_runs[1].1, output_dir.join("b"));
        assert!(output_dir
            .join("out/dev/dstack-dev-0.6.3/rootfs.img")
            .is_file());
        assert!(output_dir
            .join("out/prod/dstack-0.6.3/rootfs.img")
            .is_file());

        // Both runs produced deterministic archives and run a's archives were promoted to output_dir.
        let dev_archive = archive::archive_filename(Flavor::Dev, &dev_hash).unwrap();
        let prod_archive = archive::archive_filename(Flavor::Prod, &prod_hash).unwrap();
        assert!(output_dir.join(&dev_archive).is_file());
        assert!(output_dir.join(&prod_archive).is_file());
        assert!(output_dir.join("b").join(&dev_archive).is_file());
        assert_eq!(
            archive::sha256_file_streaming(&output_dir.join(&dev_archive)).unwrap(),
            archive::sha256_file_streaming(&output_dir.join("b").join(&dev_archive)).unwrap(),
            "run a and run b archives must have identical SHA-256"
        );

        let manifest = VmImageManifest {
            version: MANIFEST_VERSION,
            source_commit: "a".repeat(40),
            software_bundle_sha256: "d".repeat(64),
            dstack_rev: DSTACK_COMMIT.to_owned(),
            overlay_commit,
            reproducible: true,
            patches: Vec::new(),
            runtime_libs: Vec::new(),
            flavors,
        };
        write_manifest(&output_dir, &manifest).unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(output_dir.join(MANIFEST_NAME)).unwrap()).unwrap();
        assert_eq!(written["reproducible"], serde_json::Value::Bool(true));
        assert_eq!(written["flavors"]["dev"]["os_image_hash"], dev_hash);
        assert_eq!(
            written["flavors"]["dev"]["image_dir"],
            "out/dev/dstack-dev-0.6.3"
        );
        assert_eq!(written["flavors"]["dev"]["archive"], dev_archive);
        assert_eq!(
            written["flavors"]["dev"]["archive_sha256"],
            archive::sha256_file_streaming(&output_dir.join(&dev_archive)).unwrap()
        );
        assert_eq!(written["flavors"]["prod"]["os_image_hash"], prod_hash);
        assert_eq!(
            written["flavors"]["prod"]["image_dir"],
            "out/prod/dstack-0.6.3"
        );

        // `verify_image_dir` validates the built output directory against `vm-image-manifest.json`.
        let verified = archive::verify_image_dir(
            &output_dir.join("out/dev/dstack-dev-0.6.3"),
            Some(&output_dir.join(MANIFEST_NAME)),
            Some(Flavor::Dev),
        )
        .unwrap();
        assert_eq!(verified.os_image_hash, dev_hash);

        // Archive reproducibility mismatch check rejects differing archive SHA-256s.
        let mut fake_a = BTreeMap::new();
        let mut fake_b = BTreeMap::new();
        fake_a.insert(
            "dev".to_owned(),
            FlavorArtifact {
                os_image_hash: dev_hash.clone(),
                image_dir: "out/dev/dstack-dev-0.6.3".to_owned(),
                archive: Some(dev_archive.clone()),
                archive_sha256: Some("a".repeat(64)),
                files: Vec::new(),
            },
        );
        fake_b.insert(
            "dev".to_owned(),
            FlavorArtifact {
                os_image_hash: dev_hash,
                image_dir: "out/dev/dstack-dev-0.6.3".to_owned(),
                archive: Some(dev_archive),
                archive_sha256: Some("b".repeat(64)),
                files: Vec::new(),
            },
        );
        let err = compare_packed_archives(&fake_a, &fake_b)
            .expect_err("differing archive sha256 across runs must fail");
        assert!(
            err.to_string().contains("archive SHA-256 mismatch"),
            "{err}"
        );
    }

    #[test]
    fn repro_check_two_run_orchestration_fails_and_names_flavor_and_differing_file() {
        let temp = tempfile::tempdir().unwrap();
        let work_dir = temp.path().join("work");
        let dstack_src = work_dir.join("dstack-outbe");
        let output_dir = temp.path().join("output");
        fs::create_dir_all(&output_dir).unwrap();
        let overlay_commit = init_fake_overlay_repo(&dstack_src);
        let dev_hash = "1".repeat(64);
        let prod_hash = "2".repeat(64);

        let mut call_idx = 0usize;
        let err = execute_repro_builds(
            &work_dir,
            &dstack_src,
            &"a".repeat(40),
            DSTACK_COMMIT,
            &overlay_commit,
            &output_dir,
            &[Flavor::Dev, Flavor::Prod],
            "0.6.3",
            true,
            |_run_src, run_out| {
                call_idx += 1;
                let dev_bytes = if call_idx == 1 {
                    b"dev-img-a".as_slice()
                } else {
                    b"dev-img-b".as_slice()
                };
                write_fake_flavor_output(run_out, Flavor::Dev, "0.6.3", &dev_hash, dev_bytes);
                write_fake_flavor_output(run_out, Flavor::Prod, "0.6.3", &prod_hash, b"prod-img");
                Ok(())
            },
        )
        .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("flavor 'dev'"), "{msg}");
        assert!(msg.contains("rootfs.img"), "{msg}");
        assert!(!msg.contains("flavor 'prod'"), "{msg}");
        assert!(!output_dir.join("out").exists());
    }
}
