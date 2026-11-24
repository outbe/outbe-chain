//! Reproducible Outbe VM guest image builder on top of a pinned dstack revision.

use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_systemd::vm_units;
use outbe_software_bundle::{sha256_file, verify_and_extract};
use serde::{Deserialize, Serialize};

pub const DEFAULT_DSTACK_REPO: &str = "https://github.com/Dstack-TEE/dstack.git";
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
    ensure!(
        !options.output_dir.exists(),
        "output directory already exists: {}",
        options.output_dir.display()
    );

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
            extract_runtime_libs_from_toolchain(options.repo_root, &extracted_libs_dir)?;
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
    run_repro_build(
        &dstack_src,
        &output_abs,
        options.flavors,
        options.jobs,
        options.check,
    )?;

    let dstack_version = read_dstack_version(&dstack_src)?;
    let flavors =
        collect_flavor_hashes(&output_abs, options.flavors, &dstack_version, options.check)?;

    let manifest = VmImageManifest {
        version: MANIFEST_VERSION,
        source_commit,
        software_bundle_sha256: bundle_sha256,
        dstack_rev: DSTACK_COMMIT.to_owned(),
        overlay_commit,
        patches,
        runtime_libs,
        flavors,
    };
    fs::write(
        output_abs.join(MANIFEST_NAME),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(manifest)
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
        "# StrictModes is disabled because /opt/outbe-chain is owned by outbe:outbe (900:900)\n",
        "# while dev/authorized_keys is owned by root:root.\n",
        "PubkeyAuthentication yes\n",
        "PermitRootLogin prohibit-password\n",
        "StrictModes no\n",
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

const TOOLCHAIN_RUNTIME_IMAGE: &str = "outbe-project-toolchain-runtime";
const TOOLCHAIN_LIB_DIR: &str = "/usr/lib/x86_64-linux-gnu";

/// Build or reuse the `toolchain` target of `Dockerfile.project-toolchain` and
/// copy the three pinned LLVM 18 runtime libraries out of it.
fn extract_runtime_libs_from_toolchain(repo_root: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    let build_status = Command::new("docker")
        .current_dir(repo_root)
        .args([
            "build",
            "--platform",
            "linux/amd64",
            "--file",
            "Dockerfile.project-toolchain",
            "--target",
            "toolchain",
            "--tag",
            TOOLCHAIN_RUNTIME_IMAGE,
            ".",
        ])
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
            TOOLCHAIN_RUNTIME_IMAGE,
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

fn run_repro_build(
    dstack_src: &Path,
    output_dir: &Path,
    flavors: &[Flavor],
    jobs: usize,
    check: bool,
) -> Result<()> {
    let script = dstack_src.join("os/mkosi/repro-build/repro-build.sh");
    ensure!(
        script.is_file(),
        "dstack repro-build script is missing: {}",
        script.display()
    );
    let flavors_env = flavors
        .iter()
        .map(|flavor| flavor.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let mut cmd = Command::new("nice");
    cmd.args(["-n", "19"])
        .arg(&script)
        .env("FLAVORS", flavors_env)
        .env("JOBS", jobs.to_string());
    if check {
        cmd.arg("-c");
    }
    cmd.arg("-o").arg(output_dir);
    let status = cmd.status().wrap_err("run dstack repro-build.sh")?;
    ensure!(
        status.success(),
        "dstack repro-build.sh failed with {status}"
    );
    Ok(())
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

fn collect_flavor_hashes(
    output_dir: &Path,
    flavors: &[Flavor],
    dstack_version: &str,
    check: bool,
) -> Result<BTreeMap<String, FlavorArtifact>> {
    let base = if check {
        output_dir.join("a/out")
    } else {
        output_dir.join("out")
    };
    let mut map = BTreeMap::new();
    for flavor in flavors {
        let rel = format!(
            "{}/{}/{}-{dstack_version}",
            if check { "a/out" } else { "out" },
            flavor.as_str(),
            flavor.image_prefix()
        );
        let dir = base
            .join(flavor.as_str())
            .join(format!("{}-{dstack_version}", flavor.image_prefix()));
        let digest_path = dir.join("digest.txt");
        let os_image_hash = fs::read_to_string(&digest_path)
            .wrap_err_with(|| format!("read {}", digest_path.display()))?
            .trim()
            .to_owned();
        ensure!(
            !os_image_hash.is_empty(),
            "empty os_image_hash in {}",
            digest_path.display()
        );
        map.insert(
            flavor.as_str().to_owned(),
            FlavorArtifact {
                os_image_hash,
                image_dir: rel,
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
    use super::*;
    use crate::release::repository_root;

    #[test]
    fn pinned_dstack_rev_patches_and_app_compose_match_repository() {
        let root = repository_root().unwrap();
        assert_eq!(DSTACK_COMMIT.len(), 40);
        let patches = verify_pinned_patches(&root).unwrap();
        assert_eq!(patches.len(), 3);

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
        assert!(dev_sshd.contains("StrictModes no"));
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
}
