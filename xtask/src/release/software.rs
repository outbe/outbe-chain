//! Deterministic, network-neutral Outbe software bundles.

use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{ensure, Context, Result};
use outbe_operator_systemd::joining_units;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use outbe_software_bundle::sha256_file;
pub use outbe_software_bundle::{
    verify_archive, SoftwareFile, SoftwareManifest, MANIFEST_NAME, MANIFEST_VERSION,
};

use super::elf::{self, ReleaseIdentity};

const COMMON_BINARIES: &[&str] = &[
    "outbe-up",
    "outbe-chain",
    "outbe-cli",
    "outbe-keygen",
    "outbe-feeder",
    "outbe-radicle",
    "outbe-hyperlane-validator",
];
const OPERATOR_TOOL: &str = "outbe-genesis-cli";
const GENESIS_ASSETS: &str = "share/outbe-genesis";
const CRS_BUNDLE_PATH: &str = ".bb-crs/bn254_g1.dat";
const CRS_URL: &str = "https://crs.aztec.network/g1.dat";
const CRS_SIZE: usize = 4_194_368;
const CRS_SHA256: &str = "9fd9fad54cfdc61ec309e818fe7bbddad1ca7dde72cc9b1afd126b744f887078";

#[derive(Clone, Debug)]
struct BundleInput {
    archive_path: String,
    source: PathBuf,
    mode: u32,
}

pub struct PackageOptions<'a> {
    pub artifacts: &'a Path,
    pub systemd_units: &'a Path,
    pub output: &'a Path,
    pub target: &'a str,
    pub identity: &'a ReleaseIdentity,
}

pub struct BuildOptions<'a> {
    pub repo_root: &'a Path,
    pub output_dir: &'a Path,
    pub release_tag: Option<&'a str>,
    pub no_cache: bool,
    pub inside_container: bool,
}

/// Build and package the confidential-VM software bundle using the pinned Linux toolchain.
pub fn build(options: &BuildOptions<'_>) -> Result<PathBuf> {
    if options.inside_container {
        build_inside_container(options)
    } else {
        build_with_docker(options)
    }
}

/// Package pre-built Linux artifacts and the canonical service definitions.
pub fn package(options: &PackageOptions<'_>) -> Result<SoftwareManifest> {
    ensure!(
        !options.output.exists(),
        "software bundle already exists: {}",
        options.output.display()
    );
    let inputs = collect_inputs(options.artifacts, options.systemd_units)?;
    let files = inputs
        .iter()
        .map(manifest_file)
        .collect::<Result<Vec<_>>>()?;
    let manifest = SoftwareManifest {
        version: MANIFEST_VERSION,
        commit: options.identity.source_commit.clone(),
        release_tag: options.identity.release_tag.clone(),
        target: options.target.to_owned(),
        files,
    };
    manifest.validate()?;
    write_archive(
        options.output,
        &manifest,
        &inputs,
        options.identity.source_date_epoch.parse()?,
    )?;
    verify_archive(options.output)?;
    Ok(manifest)
}

fn build_with_docker(options: &BuildOptions<'_>) -> Result<PathBuf> {
    require_clean_checkout(options.repo_root)?;
    prepare_output_directory(options.output_dir)?;
    let spec = elf::load_and_validate_build_spec(
        &options
            .repo_root
            .join("release/reproducible-elf-build-v1.json"),
        Some(options.repo_root),
    )?;
    let identity = elf::resolve_release_identity(options.repo_root, options.release_tag)?;
    let values = elf::resolved_values(&spec, &identity)?;
    ensure!(values.len() == 7, "incomplete reproducible build identity");
    let output_dir = absolute_path(options.output_dir)?;
    let mut command = Command::new("docker");
    command.current_dir(options.repo_root).args([
        "build",
        "--platform",
        "linux/amd64",
        "--file",
        "Dockerfile.project-toolchain",
        "--target",
        "software-artifacts",
        "--build-arg",
        &format!("SOURCE_COMMIT={}", values[3]),
        "--build-arg",
        &format!("SOURCE_DATE_EPOCH={}", values[4]),
        "--build-arg",
        &format!("RELEASE_TAG={}", values[5]),
        "--build-arg",
        &format!("SOURCE_DESCRIBE={}", values[6]),
        "--build-arg",
        &format!("REPRODUCIBLE_RUSTFLAGS={}", values[0]),
        "--build-arg",
        &format!("REPRODUCIBLE_CFLAGS={}", values[1]),
        "--build-arg",
        &format!("REPRODUCIBLE_CXXFLAGS={}", values[2]),
        "--output",
        &format!("type=local,dest={}", output_dir.display()),
    ]);
    if options.no_cache {
        command.arg("--no-cache");
    }
    let status = command
        .arg(".")
        .status()
        .wrap_err("run pinned Docker build")?;
    ensure!(status.success(), "pinned software build failed");
    let bundle = output_dir.join(outbe_software_bundle::bundle_name(&identity.source_commit)?);
    ensure!(
        bundle.is_file(),
        "software build did not produce {}",
        bundle.display()
    );
    verify_archive(&bundle)?;
    Ok(bundle)
}

fn build_inside_container(options: &BuildOptions<'_>) -> Result<PathBuf> {
    let identity = identity_from_environment(options.release_tag)?;
    let target = "x86_64-unknown-linux-gnu";
    let status = cargo_build_command(options.repo_root, target)
        .status()
        .wrap_err("build software binaries")?;
    ensure!(status.success(), "software binary build failed");
    let staging = tempfile::tempdir().wrap_err("create software artifact staging directory")?;
    let staging_bin = staging.path().join("bin");
    let staging_systemd = staging.path().join("systemd");
    fs::create_dir(&staging_bin)?;
    let built = options
        .repo_root
        .join("target")
        .join(target)
        .join("release");
    for binary in expected_binaries() {
        let source = built.join(binary);
        let destination = staging_bin.join(binary);
        fs::copy(&source, &destination)
            .wrap_err_with(|| format!("stage release binary: {}", source.display()))?;
    }
    stage_joining_units(&staging_systemd)?;
    stage_genesis_assets(options.repo_root, &staging.path().join(GENESIS_ASSETS))?;
    stage_crs(staging.path())?;
    fs::create_dir_all(options.output_dir)?;
    let output = options
        .output_dir
        .join(outbe_software_bundle::bundle_name(&identity.source_commit)?);
    package(&PackageOptions {
        artifacts: staging.path(),
        systemd_units: &staging_systemd,
        output: &output,
        target,
        identity: &identity,
    })?;
    Ok(output)
}

fn stage_joining_units(output: &Path) -> Result<()> {
    fs::create_dir_all(output)?;
    for unit in joining_units("/usr/local/bin/outbe-up") {
        fs::write(output.join(unit.name), unit.content)?;
    }
    Ok(())
}

fn stage_genesis_assets(repo_root: &Path, output: &Path) -> Result<()> {
    copy_tree(
        &repo_root.join("bin/outbe-genesis-cli/profiles"),
        &output.join("profiles"),
    )?;
    copy_tree(
        &repo_root.join("contracts/predeploys"),
        &output.join("contracts/predeploys"),
    )
}

fn stage_crs(output: &Path) -> Result<()> {
    let destination = output.join(CRS_BUNDLE_PATH);
    fs::create_dir_all(destination.parent().expect("CRS path has a parent"))?;
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--range",
            &format!("0-{}", CRS_SIZE - 1),
            "--output",
            destination.to_str().expect("UTF-8 CRS path"),
            CRS_URL,
        ])
        .status()
        .wrap_err("download pinned CRS")?;
    ensure!(status.success(), "failed to download pinned CRS");
    let bytes = fs::read(&destination)?;
    ensure!(bytes.len() == CRS_SIZE, "downloaded CRS has the wrong size");
    ensure!(
        hex::encode(Sha256::digest(&bytes)) == CRS_SHA256,
        "downloaded CRS checksum mismatch"
    );
    fs::write(destination, bytes)?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in WalkDir::new(source) {
        let entry = entry.wrap_err_with(|| format!("walk release assets: {}", source.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(source)?;
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry.path(), &target)
            .wrap_err_with(|| format!("stage release asset: {}", entry.path().display()))?;
    }
    Ok(())
}

fn cargo_build_command(repo_root: &Path, target: &str) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(repo_root).args([
        "build",
        "--locked",
        "--release",
        "--no-default-features",
        "--target",
        target,
    ]);
    for (package, binary) in binary_targets() {
        command.args(["--package", package, "--bin", binary]);
    }
    command.args(["--features", "outbe-chain/test-protocol-overrides"]);
    command
}

fn binary_targets() -> [(&'static str, &'static str); 8] {
    [
        ("outbe-up", "outbe-up"),
        ("outbe-chain", "outbe-chain"),
        ("outbe-cli", "outbe-cli"),
        ("outbe-keygen", "outbe-keygen"),
        ("outbe-feeder", "outbe-feeder"),
        ("outbe-radicle-sidecar", "outbe-radicle"),
        ("outbe-hyperlane-validator", "outbe-hyperlane-validator"),
        ("outbe-genesis-cli", OPERATOR_TOOL),
    ]
}

fn expected_binaries() -> Vec<&'static str> {
    binary_targets()
        .into_iter()
        .map(|(_, binary)| binary)
        .collect()
}

fn identity_from_environment(requested_tag: Option<&str>) -> Result<ReleaseIdentity> {
    let required = |name: &str| {
        env::var(name).wrap_err_with(|| format!("missing container build identity: {name}"))
    };
    let source_commit = required("VERGEN_GIT_SHA")?;
    ensure!(
        is_lower_hex(&source_commit, 40),
        "invalid container source commit"
    );
    let release_tag = requested_tag
        .map(ToOwned::to_owned)
        .unwrap_or(required("RELEASE_TAG")?);
    Ok(ReleaseIdentity {
        source_commit,
        source_date_epoch: required("SOURCE_DATE_EPOCH")?,
        source_describe: required("VERGEN_GIT_DESCRIBE")?,
        release_tag,
    })
}

fn require_clean_checkout(repo_root: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .wrap_err("inspect release checkout")?;
    ensure!(output.status.success(), "git status failed");
    ensure!(
        output.stdout.is_empty(),
        "software release builds require a clean checkout"
    );
    Ok(())
}

fn prepare_output_directory(output: &Path) -> Result<()> {
    if output.exists() {
        ensure!(output.is_dir(), "release output is not a directory");
        ensure!(
            fs::read_dir(output)?.next().is_none(),
            "release output directory must be empty: {}",
            output.display()
        );
    } else {
        fs::create_dir_all(output)?;
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn collect_inputs(artifacts: &Path, systemd_units: &Path) -> Result<Vec<BundleInput>> {
    let bin = artifacts.join("bin");
    let mut inputs = COMMON_BINARIES
        .iter()
        .copied()
        .map(|name| BundleInput {
            archive_path: format!("bin/{name}"),
            source: bin.join(name),
            mode: 0o755,
        })
        .collect::<Vec<_>>();
    inputs.push(BundleInput {
        archive_path: format!("tools/{OPERATOR_TOOL}"),
        source: bin.join(OPERATOR_TOOL),
        mode: 0o755,
    });
    inputs.push(BundleInput {
        archive_path: CRS_BUNDLE_PATH.to_owned(),
        source: artifacts.join(CRS_BUNDLE_PATH),
        mode: 0o644,
    });

    let share = artifacts.join("share");
    if share.is_dir() {
        for entry in WalkDir::new(&share) {
            let entry =
                entry.wrap_err_with(|| format!("walk release share: {}", share.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(artifacts)?;
            inputs.push(BundleInput {
                archive_path: relative.to_string_lossy().replace('\\', "/"),
                source: entry.path().to_owned(),
                mode: 0o644,
            });
        }
    }

    let mut units = fs::read_dir(systemd_units)
        .wrap_err_with(|| format!("read systemd units: {}", systemd_units.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    units.sort_by_key(std::fs::DirEntry::file_name);
    for unit in units {
        let metadata = unit.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let name = unit.file_name().into_string().map_err(|_| {
            eyre::eyre!("systemd unit name is not UTF-8: {}", unit.path().display())
        })?;
        ensure!(
            name.ends_with(".service") || name.ends_with(".timer") || name.ends_with(".target"),
            "unexpected systemd asset: {name}"
        );
        inputs.push(BundleInput {
            archive_path: format!("systemd/{name}"),
            source: unit.path(),
            mode: 0o644,
        });
    }
    inputs.sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
    for input in &inputs {
        let metadata = fs::symlink_metadata(&input.source)
            .wrap_err_with(|| format!("missing release input: {}", input.source.display()))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "release input is not a regular file: {}",
            input.source.display()
        );
    }
    Ok(inputs)
}

fn manifest_file(input: &BundleInput) -> Result<SoftwareFile> {
    let metadata = fs::metadata(&input.source)?;
    Ok(SoftwareFile {
        path: input.archive_path.clone(),
        size: metadata.len(),
        sha256: sha256_file(&input.source)?,
        mode: input.mode,
    })
}

fn write_archive(
    output: &Path,
    manifest: &SoftwareManifest,
    inputs: &[BundleInput],
    source_date_epoch: i64,
) -> Result<()> {
    ensure!(
        source_date_epoch >= 0,
        "SOURCE_DATE_EPOCH must be non-negative"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)
        .wrap_err_with(|| format!("create software bundle: {}", output.display()))?;
    let mut encoder = zstd::Encoder::new(file, 19)?;
    encoder.include_checksum(true)?;
    let mut archive = tar::Builder::new(encoder);
    let manifest_bytes = canonical_json(manifest)?;
    append_bytes(
        &mut archive,
        MANIFEST_NAME,
        &manifest_bytes,
        0o644,
        source_date_epoch,
    )?;
    for input in inputs {
        let bytes = fs::read(&input.source)?;
        append_bytes(
            &mut archive,
            &input.archive_path,
            &bytes,
            input.mode,
            source_date_epoch,
        )?;
    }
    let encoder = archive.into_inner()?;
    encoder.finish()?;
    Ok(())
}

fn canonical_json(manifest: &SoftwareManifest) -> Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(manifest).wrap_err("encode software manifest")?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn append_bytes(
    archive: &mut tar::Builder<zstd::Encoder<'_, File>>,
    path: &str,
    bytes: &[u8],
    mode: u32,
    source_date_epoch: i64,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(source_date_epoch as u64);
    header.set_mode(mode);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ReleaseIdentity {
        ReleaseIdentity {
            release_tag: "commit-0123456789012345678901234567890123456789".into(),
            source_commit: "0123456789012345678901234567890123456789".into(),
            source_date_epoch: "1700000000".into(),
            source_describe: "commit-0123456789012345678901234567890123456789".into(),
        }
    }

    fn fixture(root: &Path) -> (PathBuf, PathBuf) {
        let artifacts = root.join("artifacts");
        let bin = artifacts.join("bin");
        let profiles = artifacts.join(GENESIS_ASSETS).join("profiles");
        let crs = artifacts.join(CRS_BUNDLE_PATH);
        let units = root.join("units");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&profiles).unwrap();
        fs::create_dir_all(crs.parent().unwrap()).unwrap();
        fs::create_dir_all(&units).unwrap();
        for name in COMMON_BINARIES.iter().copied().chain([OPERATOR_TOOL]) {
            fs::write(bin.join(name), format!("fixture:{name}\n")).unwrap();
        }
        fs::write(profiles.join("testnet.yaml"), "network: testnet\n").unwrap();
        fs::write(crs, b"fixture CRS\n").unwrap();
        fs::write(
            units.join("outbe-validator.service"),
            "[Service]\nExecStart=/opt/outbe-chain/outbe-chain\n",
        )
        .unwrap();
        (artifacts, units)
    }

    #[test]
    fn package_is_deterministic_and_self_verifying() {
        let temp = tempfile::tempdir().unwrap();
        let (artifacts, units) = fixture(temp.path());
        for name in [
            "outbe-unlisted-sidecar",
            "outbe-unlisted-helper",
            "outbe-unlisted-mock",
        ] {
            fs::write(artifacts.join("bin").join(name), b"stale sidecar").unwrap();
        }
        let first = temp.path().join("first.tar.zst");
        let second = temp.path().join("second.tar.zst");
        for output in [&first, &second] {
            package(&PackageOptions {
                artifacts: &artifacts,
                systemd_units: &units,
                output,
                target: "x86_64-unknown-linux-gnu",
                identity: &identity(),
            })
            .unwrap();
        }
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        let manifest = verify_archive(&first).unwrap();
        let binaries: Vec<_> = manifest
            .files
            .iter()
            .filter_map(|file| file.path.strip_prefix("bin/"))
            .collect();
        assert_eq!(
            binaries,
            [
                "outbe-chain",
                "outbe-cli",
                "outbe-feeder",
                "outbe-hyperlane-validator",
                "outbe-keygen",
                "outbe-radicle",
                "outbe-up",
            ]
        );
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == "share/outbe-genesis/profiles/testnet.yaml"));
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == CRS_BUNDLE_PATH));
    }

    #[test]
    fn software_build_uses_the_unattested_node_feature_only() {
        let command = cargo_build_command(Path::new("."), "x86_64-unknown-linux-gnu");
        let args: Vec<_> = command.get_args().collect();
        let features: Vec<_> = args
            .windows(2)
            .filter_map(|pair| (pair[0] == "--features").then_some(pair[1]))
            .collect();
        assert_eq!(
            features,
            [std::ffi::OsStr::new("outbe-chain/test-protocol-overrides")]
        );
        assert!(args.contains(&std::ffi::OsStr::new("--no-default-features")));
        assert!(command
            .get_envs()
            .all(|(key, _)| key != "OUTBE_NATIVE_DCAP"));
    }

    #[test]
    fn staged_units_are_the_canonical_embedded_assets() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("systemd");
        stage_joining_units(&output).unwrap();
        for expected in joining_units("/usr/local/bin/outbe-up") {
            assert_eq!(
                fs::read_to_string(output.join(&expected.name)).unwrap(),
                expected.content,
                "{}",
                expected.name
            );
        }
    }

    #[test]
    fn stages_genesis_runtime_assets() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join(GENESIS_ASSETS);
        stage_genesis_assets(
            Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(),
            &output,
        )
        .unwrap();
        assert!(output.join("profiles/testnet.yaml").is_file());
        assert!(output.join("profiles/devnet/pairs.toml").is_file());
        assert!(output.join("contracts/predeploys").is_dir());
    }
}
