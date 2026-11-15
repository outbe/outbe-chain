//! Deterministic, network-neutral Outbe software bundles.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
};

use clap::ValueEnum;
use eyre::{bail, ensure, Context, Result};
use outbe_operator_systemd::joining_units;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    elf::{self, ReleaseIdentity},
    sgx::canonical_json,
};

pub const MANIFEST_NAME: &str = "software-manifest.json";
pub const MANIFEST_VERSION: u32 = 1;

const COMMON_BINARIES: &[&str] = &[
    "outbe-up",
    "outbe-chain",
    "outbe-cli",
    "outbe-keygen",
    "outbe-feeder",
    "outbe-radicle",
    "outbe-hyperlane-validator",
];
const PRODUCTION_ENCLAVE: &str = "outbe-tee-enclave";
const DEVELOPMENT_ENCLAVE: &str = "outbe-tee-enclave-mock";
const OPERATOR_TOOL: &str = "outbe-genesis-cli";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SoftwareProfile {
    Production,
    Development,
}

impl SoftwareProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Development => "development",
        }
    }

    const fn enclave_binary(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_ENCLAVE,
            Self::Development => DEVELOPMENT_ENCLAVE,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwareManifest {
    pub version: u32,
    pub commit: String,
    pub release_tag: String,
    pub profile: SoftwareProfile,
    pub target: String,
    pub files: Vec<SoftwareFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SoftwareFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub mode: u32,
}

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
    pub profile: SoftwareProfile,
    pub target: &'a str,
    pub identity: &'a ReleaseIdentity,
}

pub struct BuildOptions<'a> {
    pub repo_root: &'a Path,
    pub output_dir: &'a Path,
    pub profile: SoftwareProfile,
    pub release_tag: Option<&'a str>,
    pub no_cache: bool,
    pub inside_container: bool,
}

/// Build and package one complete software profile using the pinned Linux toolchain.
pub fn build(options: &BuildOptions<'_>) -> Result<PathBuf> {
    if options.inside_container {
        build_inside_container(options)
    } else {
        build_with_docker(options)
    }
}

impl SoftwareManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == MANIFEST_VERSION,
            "unsupported software manifest version {}",
            self.version
        );
        ensure!(is_lower_hex(&self.commit, 40), "invalid source commit");
        ensure!(
            !self.release_tag.is_empty() && self.release_tag.is_ascii(),
            "release tag must be non-empty ASCII"
        );
        ensure!(
            !self.target.is_empty() && self.target.is_ascii(),
            "target must be non-empty ASCII"
        );
        let mut paths = BTreeSet::new();
        for file in &self.files {
            safe_relative_path(&file.path)?;
            ensure!(
                file.path != MANIFEST_NAME,
                "software manifest cannot list itself"
            );
            ensure!(paths.insert(&file.path), "duplicate path: {}", file.path);
            ensure!(is_lower_hex(&file.sha256, 64), "invalid file digest");
            ensure!(
                matches!(file.mode, 0o644 | 0o755),
                "unsupported mode for {}: {:o}",
                file.path,
                file.mode
            );
        }
        ensure!(!self.files.is_empty(), "software manifest has no files");
        Ok(())
    }
}

/// Package pre-built Linux artifacts and the canonical service definitions.
pub fn package(options: &PackageOptions<'_>) -> Result<SoftwareManifest> {
    ensure!(
        !options.output.exists(),
        "software bundle already exists: {}",
        options.output.display()
    );
    let inputs = collect_inputs(options.artifacts, options.systemd_units, options.profile)?;
    let files = inputs
        .iter()
        .map(manifest_file)
        .collect::<Result<Vec<_>>>()?;
    let manifest = SoftwareManifest {
        version: MANIFEST_VERSION,
        commit: options.identity.source_commit.clone(),
        release_tag: options.identity.release_tag.clone(),
        profile: options.profile,
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

/// Verify that an archive exactly matches its first-entry manifest.
pub fn verify_archive(path: &Path) -> Result<SoftwareManifest> {
    let file =
        File::open(path).wrap_err_with(|| format!("open software bundle: {}", path.display()))?;
    let decoder = zstd::Decoder::new(file).wrap_err("open software bundle zstd stream")?;
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().wrap_err("read software bundle")?;
    let mut first = entries
        .next()
        .ok_or_else(|| eyre::eyre!("software bundle is empty"))?
        .wrap_err("read software manifest entry")?;
    ensure!(
        first.path()?.as_ref() == Path::new(MANIFEST_NAME) && first.header().entry_type().is_file(),
        "software manifest must be the first regular bundle entry"
    );
    let mut raw_manifest = Vec::new();
    first.read_to_end(&mut raw_manifest)?;
    drop(first);
    let manifest: SoftwareManifest =
        serde_json::from_slice(&raw_manifest).wrap_err("parse software manifest")?;
    manifest.validate()?;

    let expected = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let mut found = BTreeSet::new();
    for entry in entries {
        let mut entry = entry.wrap_err("read software bundle entry")?;
        ensure!(
            entry.header().entry_type().is_file(),
            "software bundle contains a non-file entry"
        );
        let path = entry.path()?.into_owned();
        let path = path
            .to_str()
            .ok_or_else(|| eyre::eyre!("software bundle contains a non-UTF-8 path"))?;
        safe_relative_path(path)?;
        let file = expected
            .get(path)
            .ok_or_else(|| eyre::eyre!("undeclared software bundle file: {path}"))?;
        ensure!(
            found.insert(path.to_owned()),
            "duplicate bundle path: {path}"
        );
        ensure!(entry.size() == file.size, "size mismatch: {path}");
        ensure!(
            entry.header().mode()? & 0o777 == file.mode,
            "mode mismatch: {path}"
        );
        ensure!(
            digest_reader(&mut entry)? == file.sha256,
            "digest mismatch: {path}"
        );
    }
    ensure!(
        found.len() == expected.len(),
        "software bundle is missing declared files"
    );
    Ok(manifest)
}

pub fn bundle_name(profile: SoftwareProfile, commit: &str) -> Result<String> {
    ensure!(is_lower_hex(commit, 40), "invalid source commit");
    Ok(format!(
        "outbe-software-bundle-v{MANIFEST_VERSION}-{}-{commit}.tar.zst",
        profile.as_str()
    ))
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
        "--build-arg",
        &format!("SOFTWARE_PROFILE={}", options.profile.as_str()),
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
    let bundle = output_dir.join(bundle_name(options.profile, &identity.source_commit)?);
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
    run_cargo_build(options.repo_root, options.profile, target)?;
    let staging = tempfile::tempdir().wrap_err("create software artifact staging directory")?;
    let staging_bin = staging.path().join("bin");
    let staging_systemd = staging.path().join("systemd");
    fs::create_dir(&staging_bin)?;
    let built = options
        .repo_root
        .join("target")
        .join(target)
        .join("release");
    for binary in expected_binaries(options.profile) {
        let source = built.join(binary);
        let destination = staging_bin.join(binary);
        fs::copy(&source, &destination)
            .wrap_err_with(|| format!("stage release binary: {}", source.display()))?;
    }
    stage_joining_units(&staging_systemd)?;
    fs::create_dir_all(options.output_dir)?;
    let output = options
        .output_dir
        .join(bundle_name(options.profile, &identity.source_commit)?);
    package(&PackageOptions {
        artifacts: staging.path(),
        systemd_units: &staging_systemd,
        output: &output,
        profile: options.profile,
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

fn run_cargo_build(repo_root: &Path, profile: SoftwareProfile, target: &str) -> Result<()> {
    let mut command = Command::new("cargo");
    command.current_dir(repo_root).args([
        "build",
        "--locked",
        "--release",
        "--no-default-features",
        "--target",
        target,
    ]);
    for (package, binary) in binary_targets(profile) {
        command.args(["--package", package, "--bin", binary]);
    }
    let features = match profile {
        SoftwareProfile::Production => "outbe-tee-enclave/production-dcap-release",
        SoftwareProfile::Development => {
            "outbe-chain/test-protocol-overrides,outbe-tee-enclave/mock"
        }
    };
    command.args(["--features", features]);
    if profile == SoftwareProfile::Production {
        command.env("OUTBE_NATIVE_DCAP", "require");
    }
    let status = command.status().wrap_err("build software binaries")?;
    ensure!(status.success(), "software binary build failed");
    Ok(())
}

fn binary_targets(profile: SoftwareProfile) -> Vec<(&'static str, &'static str)> {
    let mut targets = vec![
        ("outbe-up", "outbe-up"),
        ("outbe-chain", "outbe-chain"),
        ("outbe-cli", "outbe-cli"),
        ("outbe-keygen", "outbe-keygen"),
        ("outbe-feeder", "outbe-feeder"),
        ("outbe-radicle-sidecar", "outbe-radicle"),
        ("outbe-hyperlane-validator", "outbe-hyperlane-validator"),
        ("outbe-genesis-cli", OPERATOR_TOOL),
    ];
    targets.push(("outbe-tee-enclave", profile.enclave_binary()));
    targets
}

fn expected_binaries(profile: SoftwareProfile) -> Vec<&'static str> {
    binary_targets(profile)
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

fn collect_inputs(
    artifacts: &Path,
    systemd_units: &Path,
    profile: SoftwareProfile,
) -> Result<Vec<BundleInput>> {
    let bin = artifacts.join("bin");
    let mut inputs = COMMON_BINARIES
        .iter()
        .copied()
        .chain(std::iter::once(profile.enclave_binary()))
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
        sha256: digest_file(&input.source)?,
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

fn digest_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    digest_reader(file)
}

fn digest_reader(mut reader: impl Read) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn safe_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe bundle path: {value}");
    }
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

    fn fixture(root: &Path, profile: SoftwareProfile) -> (PathBuf, PathBuf) {
        let artifacts = root.join("artifacts");
        let bin = artifacts.join("bin");
        let units = root.join("units");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&units).unwrap();
        for name in COMMON_BINARIES
            .iter()
            .copied()
            .chain([profile.enclave_binary(), OPERATOR_TOOL])
        {
            fs::write(bin.join(name), format!("fixture:{name}\n")).unwrap();
        }
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
        let profile = SoftwareProfile::Production;
        let (artifacts, units) = fixture(temp.path(), profile);
        let first = temp.path().join("first.tar.zst");
        let second = temp.path().join("second.tar.zst");
        for output in [&first, &second] {
            package(&PackageOptions {
                artifacts: &artifacts,
                systemd_units: &units,
                output,
                profile,
                target: "x86_64-unknown-linux-gnu",
                identity: &identity(),
            })
            .unwrap();
        }
        assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
        let manifest = verify_archive(&first).unwrap();
        assert_eq!(manifest.profile, SoftwareProfile::Production);
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == "bin/outbe-hyperlane-validator"));
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == "bin/outbe-tee-enclave"));
        assert!(!manifest.files.iter().any(|file| file.path.contains("mock")));
    }

    #[test]
    fn development_bundle_contains_only_the_mock_enclave() {
        let temp = tempfile::tempdir().unwrap();
        let profile = SoftwareProfile::Development;
        let (artifacts, units) = fixture(temp.path(), profile);
        let output = temp.path().join("development.tar.zst");
        let manifest = package(&PackageOptions {
            artifacts: &artifacts,
            systemd_units: &units,
            output: &output,
            profile,
            target: "x86_64-unknown-linux-gnu",
            identity: &identity(),
        })
        .unwrap();
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == "bin/outbe-tee-enclave-mock"));
        assert!(!manifest
            .files
            .iter()
            .any(|file| file.path == "bin/outbe-tee-enclave"));
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
}
