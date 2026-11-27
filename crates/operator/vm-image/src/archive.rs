//! Distribution archive (`.tar.zst`) packing, verification, and image directory integrity checks.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Flavor;

mod extract;
mod image_dir;

pub use extract::{
    verify_archive_and_extract, verify_archive_and_extract_cancellable,
    verify_archive_file_and_extract_cancellable, ArchiveExpectation,
};
pub(crate) use image_dir::collect_image_files;
pub use image_dir::{verify_image_dir, verify_image_dir_against_archive_manifest};

pub const ARCHIVE_MANIFEST_NAME: &str = "image-archive-manifest.json";
pub const ARCHIVE_MANIFEST_VERSION: u32 = 1;
pub const MAX_ARCHIVE_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_ARCHIVE_FILES: usize = 128;
/// Maximum total extracted and decompressed stream size for one guest OS image archive (4 GiB).
pub const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Maximum decompressed tar header/extension metadata bytes before a single entry is yielded.
const MAX_ARCHIVE_HEADER_METADATA_BYTES: u64 = 1024 * 1024;

/// Build-time-only files excluded from VMM distribution archives (not used by dstack VMM).
const EXCLUDED_DISTRIBUTION_FILES: &[&str] = &["disk.raw"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageFileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageArchiveManifest {
    pub version: u32,
    pub flavor: Flavor,
    pub os_image_hash: String,
    pub source_commit: String,
    pub dstack_rev: String,
    pub overlay_commit: String,
    pub files: Vec<ImageFileEntry>,
}

#[derive(Clone, Debug)]
pub struct ArchiveBuildMetadata<'a> {
    pub flavor: Flavor,
    pub source_commit: &'a str,
    pub dstack_rev: &'a str,
    pub overlay_commit: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedImageDir {
    pub flavor: Option<Flavor>,
    pub os_image_hash: String,
    pub verified_files: usize,
    pub manifest_source: String,
}

impl ImageArchiveManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == ARCHIVE_MANIFEST_VERSION,
            "unsupported image archive manifest version {}",
            self.version
        );
        ensure!(
            is_lower_hex(&self.os_image_hash, 64),
            "invalid os_image_hash in image archive manifest"
        );
        ensure!(
            !self.source_commit.is_empty() && self.source_commit.is_ascii(),
            "source_commit must be non-empty ASCII"
        );
        ensure!(
            !self.dstack_rev.is_empty() && self.dstack_rev.is_ascii(),
            "dstack_rev must be non-empty ASCII"
        );
        ensure!(
            !self.overlay_commit.is_empty() && self.overlay_commit.is_ascii(),
            "overlay_commit must be non-empty ASCII"
        );
        validate_file_entries(&self.files)?;
        ensure!(
            self.files.iter().any(|f| f.path == "digest.txt"),
            "image archive manifest must include digest.txt"
        );
        ensure!(
            self.files.iter().any(|f| f.path == "metadata.json"),
            "image archive manifest must include metadata.json"
        );
        Ok(())
    }
}

pub fn archive_filename(flavor: Flavor, os_image_hash: &str) -> Result<String> {
    ensure!(
        is_lower_hex(os_image_hash, 64),
        "invalid os_image_hash (expected 64 lowercase hex chars): {os_image_hash}"
    );
    Ok(format!(
        "outbe-vm-image-v{ARCHIVE_MANIFEST_VERSION}-{}-{}.tar.zst",
        flavor.as_str(),
        &os_image_hash[..16]
    ))
}

/// Pack a built flavor image directory into a deterministic `.tar.zst` distribution archive whose
/// first entry is `image-archive-manifest.json`. Streams files with fixed 64 KiB buffers and
/// publishes atomically without clobbering existing files or symlinks.
pub fn pack_flavor_archive(
    image_dir: &Path,
    meta: &ArchiveBuildMetadata<'_>,
    output_archive: &Path,
) -> Result<(ImageArchiveManifest, String)> {
    ensure!(
        image_dir.is_dir(),
        "image directory does not exist: {}",
        image_dir.display()
    );
    match fs::symlink_metadata(output_archive) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .wrap_err_with(|| format!("failed to stat {}", output_archive.display()));
        }
        Ok(_) => {
            return Err(eyre::eyre!(
                "image archive already exists: {}",
                output_archive.display()
            ));
        }
    }

    let os_image_hash = read_digest_txt(image_dir)?;
    verify_metadata_json(image_dir)?;
    verify_sha256sum_txt_if_present(image_dir)?;
    let entries = collect_image_files(image_dir)?;
    let manifest = ImageArchiveManifest {
        version: ARCHIVE_MANIFEST_VERSION,
        flavor: meta.flavor,
        os_image_hash,
        source_commit: meta.source_commit.to_owned(),
        dstack_rev: meta.dstack_rev.to_owned(),
        overlay_commit: meta.overlay_commit.to_owned(),
        files: entries,
    };
    manifest.validate()?;

    let manifest_bytes = format!("{}\n", serde_json::to_string_pretty(&manifest)?).into_bytes();

    let parent = output_archive
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let tmp = tempfile::Builder::new()
        .prefix(".outbe-vm-image-archive.")
        .tempfile_in(parent)?;
    let tmp_path = tmp.path().to_path_buf();

    {
        let file = tmp.reopen()?;
        let mut encoder = zstd::Encoder::new(file, 3)?;
        encoder.include_checksum(true)?;
        let mut tar = tar::Builder::new(encoder);

        append_tar_bytes(&mut tar, ARCHIVE_MANIFEST_NAME, &manifest_bytes, 0o644)?;
        for entry in &manifest.files {
            let src_path = image_dir.join(&entry.path);
            append_tar_file(&mut tar, &entry.path, &src_path, entry.size, entry.mode)?;
        }
        let encoder = tar.into_inner()?;
        let mut inner_file = encoder.finish()?;
        inner_file.flush()?;
        inner_file.sync_all()?;
    }

    let archive_sha256 = sha256_file_streaming(&tmp_path)?;
    tmp.persist_noclobber(output_archive)
        .map_err(|err| err.error)
        .wrap_err_with(|| format!("publish image archive to {}", output_archive.display()))?;
    if let Ok(parent_dir) = File::open(parent) {
        let _ = parent_dir.sync_all();
    }

    Ok((manifest, archive_sha256))
}

fn validate_file_entries(entries: &[ImageFileEntry]) -> Result<()> {
    ensure!(!entries.is_empty(), "image manifest lists no files");
    ensure!(
        entries.len() <= MAX_ARCHIVE_FILES,
        "image manifest lists {} files, exceeding limit {MAX_ARCHIVE_FILES}",
        entries.len()
    );
    let mut seen = BTreeSet::new();
    let mut total_size: u64 = 0;
    for entry in entries {
        safe_relative_path(&entry.path)?;
        ensure!(
            entry.path != ARCHIVE_MANIFEST_NAME,
            "image manifest cannot list {ARCHIVE_MANIFEST_NAME}"
        );
        ensure!(
            seen.insert(entry.path.as_str()),
            "duplicate file in image manifest: {}",
            entry.path
        );
        ensure!(
            is_lower_hex(&entry.sha256, 64),
            "invalid SHA-256 for {}",
            entry.path
        );
        ensure!(
            entry.mode & !0o777 == 0 && entry.mode & 0o022 == 0,
            "unsafe mode {:04o} for {}",
            entry.mode,
            entry.path
        );
        total_size = total_size
            .checked_add(entry.size)
            .ok_or_else(|| eyre::eyre!("image manifest size overflow"))?;
        ensure!(
            total_size <= MAX_EXTRACTED_BYTES,
            "image manifest total size ({total_size}) exceeds limit {MAX_EXTRACTED_BYTES}"
        );
    }
    Ok(())
}

pub(crate) fn read_digest_txt(image_dir: &Path) -> Result<String> {
    let path = image_dir.join("digest.txt");
    ensure!(
        path.is_file(),
        "image directory is missing digest.txt: {}",
        path.display()
    );
    let digest = fs::read_to_string(&path)
        .wrap_err_with(|| format!("read {}", path.display()))?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        is_lower_hex(&digest, 64),
        "digest.txt must contain 64 lowercase hex characters (got '{}')",
        digest
    );
    Ok(digest)
}

fn verify_metadata_json(image_dir: &Path) -> Result<()> {
    let path = image_dir.join("metadata.json");
    ensure!(
        path.is_file(),
        "image directory is missing metadata.json: {}",
        path.display()
    );
    let parsed: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).wrap_err_with(|| format!("read {}", path.display()))?,
    )
    .wrap_err_with(|| format!("invalid JSON in {}", path.display()))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| eyre::eyre!("metadata.json must be a JSON object"))?;
    for key in ["bios", "bios-sev", "kernel", "initrd", "rootfs"] {
        if let Some(val) = obj.get(key).and_then(serde_json::Value::as_str) {
            safe_relative_path(val)?;
            let target = image_dir.join(val);
            let meta = fs::symlink_metadata(&target)
                .wrap_err_with(|| format!("metadata.json references missing {key} file: {val}"))?;
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "metadata.json {key} is not a regular file: {val}"
            );
        }
    }
    Ok(())
}

fn verify_sha256sum_txt_if_present(image_dir: &Path) -> Result<()> {
    let path = image_dir.join("sha256sum.txt");
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&path).wrap_err_with(|| format!("read {}", path.display()))?;
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let expected_hex = parts
            .next()
            .ok_or_else(|| eyre::eyre!("malformed sha256sum.txt line {}", idx + 1))?
            .to_ascii_lowercase();
        let file_name = parts
            .next()
            .ok_or_else(|| eyre::eyre!("malformed sha256sum.txt line {}", idx + 1))?;
        ensure!(
            parts.next().is_none() && is_lower_hex(&expected_hex, 64),
            "malformed sha256sum.txt line {}",
            idx + 1
        );
        safe_relative_path(file_name)?;
        let actual = sha256_file_streaming(&image_dir.join(file_name))?;
        ensure!(
            actual == expected_hex,
            "sha256sum.txt mismatch for {file_name}: expected {expected_hex}, got {actual}"
        );
    }
    Ok(())
}

fn append_tar_bytes<W: Write>(
    tar: &mut tar::Builder<W>,
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
    tar.append_data(&mut header, path, bytes)?;
    Ok(())
}

fn append_tar_file<W: Write>(
    tar: &mut tar::Builder<W>,
    rel_path: &str,
    src_path: &Path,
    size: u64,
    mode: u32,
) -> Result<()> {
    let mut file = File::open(src_path)
        .wrap_err_with(|| format!("open {} for image archive", src_path.display()))?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_mode(mode);
    header.set_size(size);
    header.set_cksum();
    tar.append_data(&mut header, rel_path, &mut file)?;
    Ok(())
}

pub fn sha256_file_streaming(path: &Path) -> Result<String> {
    let never = AtomicBool::new(false);
    sha256_file_streaming_cancellable(path, &never)
}

pub fn sha256_file_streaming_cancellable(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut file =
        File::open(path).wrap_err_with(|| format!("open {} for sha256", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "file hashing canceled by signal"
        );
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn normalize_sha256(raw: &str, label: &str) -> Result<String> {
    let normalized = raw.trim().to_ascii_lowercase();
    ensure!(
        is_lower_hex(&normalized, 64),
        "{label} must be a 64-character hex SHA-256 digest (got '{}')",
        raw.trim()
    );
    Ok(normalized)
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe relative path in image archive: {value}");
    }
    Ok(path.to_path_buf())
}

pub(crate) fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;
