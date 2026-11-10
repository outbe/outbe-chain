use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::validator::config::sha256;

pub const MANIFEST_NAME: &str = "operator-manifest.json";
const MANIFEST_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorManifest {
    pub version: u32,
    pub files: Vec<ManifestFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub mode: u32,
}

pub struct ExtractedBundle {
    pub directory: tempfile::TempDir,
    pub archive_sha256: String,
}

impl OperatorManifest {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .wrap_err_with(|| format!("failed to read bundle manifest: {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes).wrap_err("invalid bundle manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == MANIFEST_VERSION,
            "unsupported operator manifest version {}",
            self.version
        );
        ensure!(!self.files.is_empty(), "operator manifest has no files");
        let mut paths = BTreeSet::new();
        for file in &self.files {
            let path = safe_relative_path(&file.path)?;
            ensure!(
                path != Path::new(MANIFEST_NAME),
                "operator manifest cannot list itself"
            );
            ensure!(paths.insert(path), "duplicate bundle path: {}", file.path);
            ensure!(
                valid_digest(&file.sha256),
                "invalid digest for {}",
                file.path
            );
            ensure!(
                file.mode & !0o777 == 0 && file.mode & 0o022 == 0,
                "unsafe mode for {}: {:o}",
                file.path,
                file.mode
            );
        }
        Ok(())
    }

    pub fn verify_directory(&self, root: &Path) -> Result<()> {
        for expected in &self.files {
            let path = root.join(&expected.path);
            let metadata = path
                .symlink_metadata()
                .wrap_err_with(|| format!("missing installed bundle file: {}", path.display()))?;
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "installed bundle path is not a regular file: {}",
                path.display()
            );
            ensure!(
                metadata.len() == expected.size,
                "size mismatch: {}",
                expected.path
            );
            ensure!(
                metadata.permissions().mode() & 0o777 == expected.mode,
                "mode mismatch: {}",
                expected.path
            );
            ensure!(
                digest_file(&path)? == expected.sha256,
                "checksum mismatch: {}",
                expected.path
            );
        }
        Ok(())
    }
}

pub fn verify_and_extract(archive: &Path, parent: &Path) -> Result<ExtractedBundle> {
    let archive_sha256 = digest_file(archive)?;
    let mut tar = tar::Archive::new(open_archive(archive)?);
    let mut entries = tar.entries().wrap_err("failed to read operator bundle")?;
    let mut manifest_entry = entries
        .next()
        .ok_or_else(|| eyre::eyre!("operator bundle is empty"))?
        .wrap_err("failed to read operator manifest entry")?;
    ensure!(
        manifest_entry.path()?.as_ref() == Path::new(MANIFEST_NAME)
            && manifest_entry.header().entry_type().is_file()
            && manifest_entry.size() <= MAX_MANIFEST_BYTES,
        "operator manifest must be the first regular bundle entry"
    );
    let mut raw_manifest = Vec::new();
    manifest_entry
        .read_to_end(&mut raw_manifest)
        .wrap_err("failed to read operator manifest")?;
    drop(manifest_entry);
    let manifest: OperatorManifest =
        serde_json::from_slice(&raw_manifest).wrap_err("invalid operator manifest")?;
    manifest.validate()?;

    let directory = tempfile::Builder::new()
        .prefix(".outbe-prepare-")
        .tempdir_in(parent)
        .wrap_err_with(|| format!("failed to create staging directory in {}", parent.display()))?;
    write_file(&directory.path().join(MANIFEST_NAME), &raw_manifest, 0o644)?;

    let expected: BTreeMap<&str, &ManifestFile> = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let mut found = BTreeSet::new();
    for entry in entries {
        let mut entry = entry.wrap_err("failed to read operator bundle entry")?;
        ensure!(
            entry.header().entry_type().is_file(),
            "operator bundle contains a non-file entry"
        );
        let path = entry.path()?.into_owned();
        let path_text = path
            .to_str()
            .ok_or_else(|| eyre::eyre!("operator bundle contains a non-UTF-8 path"))?;
        safe_relative_path(path_text)?;
        let expected = expected
            .get(path_text)
            .ok_or_else(|| eyre::eyre!("undeclared operator bundle file: {path_text}"))?;
        ensure!(
            found.insert(path_text.to_owned()),
            "duplicate bundle file: {path_text}"
        );
        ensure!(entry.size() == expected.size, "size mismatch: {path_text}");
        ensure!(
            entry.header().mode()? & 0o777 == expected.mode,
            "mode mismatch: {path_text}"
        );
        let output = directory.path().join(&path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .wrap_err_with(|| format!("failed to create {}", output.display()))?;
        let digest = copy_digest(&mut entry, &mut file)?;
        file.sync_all()
            .wrap_err_with(|| format!("failed to sync {}", output.display()))?;
        fs::set_permissions(&output, fs::Permissions::from_mode(expected.mode))?;
        ensure!(digest == expected.sha256, "checksum mismatch: {path_text}");
    }
    ensure!(
        found.len() == expected.len(),
        "operator bundle is missing declared files"
    );
    Ok(ExtractedBundle {
        directory,
        archive_sha256,
    })
}

pub fn digest_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .wrap_err_with(|| format!("failed to open for hashing: {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).wrap_err("failed to hash file")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn copy_digest(reader: &mut impl Read, writer: &mut impl Write) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn open_archive(path: &Path) -> Result<Box<dyn Read>> {
    let file = File::open(path)
        .wrap_err_with(|| format!("failed to open operator bundle: {}", path.display()))?;
    if path.extension().is_some_and(|extension| extension == "zst") {
        return Ok(Box::new(
            zstd::Decoder::new(file).wrap_err("failed to open zstd operator bundle")?,
        ));
    }
    Ok(Box::new(file))
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe bundle path: {value}");
    }
    Ok(path.to_path_buf())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rejects_paths_that_can_escape() {
        let manifest = OperatorManifest {
            version: MANIFEST_VERSION,
            files: vec![ManifestFile {
                path: "../keys/evm-key.hex".into(),
                size: 1,
                sha256: sha256(b"x"),
                mode: 0o600,
            }],
        };
        assert!(manifest.validate().is_err());
    }
}
