//! Manifest and verification contract for immutable Outbe software bundles.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path},
};

use clap::ValueEnum;
use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST_NAME: &str = "software-manifest.json";
pub const MANIFEST_VERSION: u32 = 1;

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

    pub const fn enclave_binary(self) -> &'static str {
        match self {
            Self::Production => "outbe-tee-enclave",
            Self::Development => "outbe-tee-enclave-mock",
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

/// Verify that an archive exactly matches its first-entry manifest.
pub fn verify_archive(path: &Path) -> Result<SoftwareManifest> {
    verify_or_extract(path, None)
}

/// Verify an archive and extract its declared regular files below `output`.
pub fn verify_and_extract(path: &Path, output: &Path) -> Result<SoftwareManifest> {
    ensure!(output.is_dir(), "extract destination is not a directory");
    verify_or_extract(path, Some(output))
}

fn verify_or_extract(path: &Path, output: Option<&Path>) -> Result<SoftwareManifest> {
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
        let declared = expected
            .get(path)
            .ok_or_else(|| eyre::eyre!("undeclared software bundle file: {path}"))?;
        ensure!(
            found.insert(path.to_owned()),
            "duplicate bundle path: {path}"
        );
        ensure!(entry.size() == declared.size, "size mismatch: {path}");
        ensure!(
            entry.header().mode()? & 0o777 == declared.mode,
            "mode mismatch: {path}"
        );

        let actual_digest = match output {
            Some(root) => extract_entry(&mut entry, root, path, declared.mode)?,
            None => digest_reader(&mut entry)?,
        };
        ensure!(actual_digest == declared.sha256, "digest mismatch: {path}");
    }
    ensure!(
        found.len() == expected.len(),
        "software bundle is missing declared files"
    );
    Ok(manifest)
}

fn extract_entry(
    entry: &mut tar::Entry<'_, impl Read>,
    root: &Path,
    relative: &str,
    mode: u32,
) -> Result<String> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::options().write(true).create_new(true).open(&path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = entry.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
    }
    file.sync_all()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
    Ok(hex::encode(digest.finalize()))
}

pub fn bundle_name(profile: SoftwareProfile, commit: &str) -> Result<String> {
    ensure!(is_lower_hex(commit, 40), "invalid source commit");
    Ok(format!(
        "outbe-software-bundle-v{MANIFEST_VERSION}-{}-{commit}.tar.zst",
        profile.as_str()
    ))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    digest_reader(File::open(path)?)
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
