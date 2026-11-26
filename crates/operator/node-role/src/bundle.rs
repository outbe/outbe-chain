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
use crate::config::sha256;

pub(crate) const MANIFEST_NAME: &str = "operator-manifest.json";
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

pub(crate) struct ExtractedBundle {
    // Field order matters: `directory` drops (`remove_dir_all`) BEFORE `_dir_lock` releases the
    // exclusive directory `flock`, preventing `--cleanup-stale` from racing with teardown.
    pub(crate) directory: tempfile::TempDir,
    pub(crate) _dir_lock: rustix::fd::OwnedFd,
    pub(crate) archive_sha256: String,
}

#[derive(Clone, Debug)]
pub struct BundleInput {
    pub path: String,
    pub source: PathBuf,
    pub mode: u32,
}

impl OperatorManifest {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .wrap_err_with(|| format!("failed to read bundle manifest: {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes).wrap_err("invalid bundle manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn validate(&self) -> Result<()> {
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

    pub(crate) fn verify_directory(&self, root: &Path) -> Result<()> {
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

pub(crate) fn verify_and_extract(archive: &Path, parent: &Path) -> Result<ExtractedBundle> {
    let never = std::sync::atomic::AtomicBool::new(false);
    verify_and_extract_cancellable(archive, parent, &never)
}

pub(crate) fn verify_and_extract_cancellable(
    archive: &Path,
    parent: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<ExtractedBundle> {
    ensure!(
        !cancel.load(std::sync::atomic::Ordering::Relaxed),
        "bundle extraction canceled by signal"
    );
    let archive_sha256 = digest_file_cancellable(archive, cancel)?;
    ensure!(
        !cancel.load(std::sync::atomic::Ordering::Relaxed),
        "bundle extraction canceled by signal"
    );
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

    let (directory, dir_lock) = create_locked_prepare_dir(parent)?;
    write_file(&directory.path().join(MANIFEST_NAME), &raw_manifest, 0o644)?;

    let expected: BTreeMap<&str, &ManifestFile> = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let mut found = BTreeSet::new();
    for entry in entries {
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "bundle extraction canceled by signal"
        );
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
        let digest = copy_digest_cancellable(&mut entry, &mut file, cancel)?;
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
        _dir_lock: dir_lock,
        archive_sha256,
    })
}

fn create_locked_prepare_dir(parent: &Path) -> Result<(tempfile::TempDir, rustix::fd::OwnedFd)> {
    use rustix::fs::{flock, openat, FlockOperation, Mode, OFlags, CWD};

    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("failed to create staging parent {}", parent.display()))?;
    let parent_fd = openat(
        CWD,
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .wrap_err_with(|| format!("open staging parent {}", parent.display()))?;
    flock(&parent_fd, FlockOperation::LockShared)
        .map_err(std::io::Error::from)
        .wrap_err_with(|| format!("lock staging parent {}", parent.display()))?;

    let directory = tempfile::Builder::new()
        .prefix(".outbe-prepare-")
        .tempdir_in(parent)
        .wrap_err_with(|| format!("failed to create staging directory in {}", parent.display()))?;
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    let dir_lock = openat(
        CWD,
        directory.path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .wrap_err_with(|| format!("open staging dir {} for flock", directory.path().display()))?;
    flock(&dir_lock, FlockOperation::LockExclusive)
        .map_err(std::io::Error::from)
        .wrap_err_with(|| format!("flock staging dir {}", directory.path().display()))?;
    drop(parent_fd);
    Ok((directory, dir_lock))
}

fn digest_file_cancellable(path: &Path, cancel: &std::sync::atomic::AtomicBool) -> Result<String> {
    let mut file =
        File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "bundle verification canceled by signal"
        );
        let count = file.read(&mut buffer).wrap_err("failed to hash file")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn copy_digest_cancellable(
    reader: &mut impl Read,
    writer: &mut impl Write,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        ensure!(
            !cancel.load(std::sync::atomic::Ordering::Relaxed),
            "bundle extraction canceled by signal"
        );
        let count = reader
            .read(&mut buffer)
            .wrap_err("failed to read bundle entry")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        writer
            .write_all(&buffer[..count])
            .wrap_err("failed to write bundle entry")?;
    }
    Ok(hex::encode(digest.finalize()))
}

pub(crate) fn digest_file(path: &Path) -> Result<String> {
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

pub fn create_archive(output: &Path, mut inputs: Vec<BundleInput>) -> Result<OperatorManifest> {
    ensure!(
        !output.exists(),
        "operator bundle already exists: {}",
        output.display()
    );
    inputs.sort_by(|left, right| left.path.cmp(&right.path));
    let files = inputs
        .iter()
        .map(|input| {
            let source = fs::symlink_metadata(&input.source).wrap_err_with(|| {
                format!("missing operator bundle input: {}", input.source.display())
            })?;
            ensure!(
                source.is_file() && !source.file_type().is_symlink(),
                "operator bundle input is not a regular file: {}",
                input.source.display()
            );
            Ok(ManifestFile {
                path: input.path.clone(),
                size: source.len(),
                sha256: digest_file(&input.source)?,
                mode: input.mode,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let manifest = OperatorManifest {
        version: MANIFEST_VERSION,
        files,
    };
    manifest.validate()?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let mut encoder = zstd::Encoder::new(file, 19)?;
    encoder.include_checksum(true)?;
    let mut archive = tar::Builder::new(encoder);
    append_archive_bytes(
        &mut archive,
        MANIFEST_NAME,
        &serde_json::to_vec_pretty(&manifest)?,
        0o644,
    )?;
    for input in inputs {
        append_archive_bytes(
            &mut archive,
            &input.path,
            &fs::read(&input.source)?,
            input.mode,
        )?;
    }
    archive.into_inner()?.finish()?;
    Ok(manifest)
}

fn append_archive_bytes(
    archive: &mut tar::Builder<zstd::Encoder<'_, File>>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_mode(mode);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)?;
    Ok(())
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

    #[test]
    fn created_archive_verifies_and_extracts() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"payload").unwrap();
        let archive = root.path().join("bundle.tar.zst");
        create_archive(
            &archive,
            vec![BundleInput {
                path: "bin/tool".into(),
                source,
                mode: 0o755,
            }],
        )
        .unwrap();
        let parent = root.path().join("extract");
        fs::create_dir(&parent).unwrap();
        let extracted = verify_and_extract(&archive, &parent).unwrap();
        assert_eq!(
            fs::read(extracted.directory.path().join("bin/tool")).unwrap(),
            b"payload"
        );
    }
}
