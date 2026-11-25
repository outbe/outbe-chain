//! Distribution archive (`.tar.zst`) packing, verification, and image directory integrity checks.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{Flavor, VmImageManifest};

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

struct ArchiveStreamBudget {
    total_read: u64,
    max_total: u64,
    entry_meta_read: u64,
    max_entry_meta: u64,
    counting_meta: bool,
}

/// Reader placed *below* `tar::Archive` so every decompressed byte — including headers, padding,
/// GNU longname/longlink extensions, PAX extended headers, and payloads — is bounded before `tar`
/// can allocate or parse it.
struct BoundedArchiveReader<'a, R: Read> {
    inner: R,
    budget: Rc<RefCell<ArchiveStreamBudget>>,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for BoundedArchiveReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "image archive extraction canceled by signal",
            ));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let allowed_cap = {
            let b = self.budget.borrow();
            let total_rem = b.max_total.saturating_sub(b.total_read);
            let limit = if b.counting_meta {
                let meta_rem = b.max_entry_meta.saturating_sub(b.entry_meta_read);
                total_rem.min(meta_rem)
            } else {
                total_rem
            };
            limit.saturating_add(1).min(buf.len() as u64).max(1) as usize
        };

        let n = self.inner.read(&mut buf[..allowed_cap])?;
        if n == 0 {
            return Ok(0);
        }

        let mut b = self.budget.borrow_mut();
        b.total_read = b.total_read.saturating_add(n as u64);
        if b.total_read > b.max_total {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "image archive decompressed stream exceeds maximum {} bytes",
                    b.max_total
                ),
            ));
        }
        if b.counting_meta {
            b.entry_meta_read = b.entry_meta_read.saturating_add(n as u64);
            if b.entry_meta_read > b.max_entry_meta {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "image archive entry header/metadata exceeds maximum {} bytes",
                        b.max_entry_meta
                    ),
                ));
            }
        }
        Ok(n)
    }
}

/// Verify the raw archive SHA-256 *before* decompression, then extract and verify every declared
/// file into `dest_dir` with hard caps on file count, header metadata, decompressed stream bytes,
/// and extracted bytes.
pub fn verify_archive_and_extract(
    archive_path: &Path,
    expected_archive_sha256: &str,
    expected_os_image_hash: Option<&str>,
    dest_dir: &Path,
) -> Result<ImageArchiveManifest> {
    let never = AtomicBool::new(false);
    verify_archive_and_extract_cancellable(
        archive_path,
        expected_archive_sha256,
        expected_os_image_hash,
        dest_dir,
        &never,
    )
}

/// Same as [`verify_archive_and_extract`], honoring a cooperative `cancel` flag during hashing,
/// decompression, and file extraction.
pub fn verify_archive_and_extract_cancellable(
    archive_path: &Path,
    expected_archive_sha256: &str,
    expected_os_image_hash: Option<&str>,
    dest_dir: &Path,
    cancel: &AtomicBool,
) -> Result<ImageArchiveManifest> {
    verify_archive_and_extract_with_limits(
        archive_path,
        expected_archive_sha256,
        expected_os_image_hash,
        dest_dir,
        MAX_ARCHIVE_FILES,
        MAX_EXTRACTED_BYTES,
        MAX_ARCHIVE_HEADER_METADATA_BYTES,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn verify_archive_and_extract_with_limits(
    archive_path: &Path,
    expected_archive_sha256: &str,
    expected_os_image_hash: Option<&str>,
    dest_dir: &Path,
    max_files: usize,
    max_extracted_bytes: u64,
    max_entry_meta_bytes: u64,
    cancel: &AtomicBool,
) -> Result<ImageArchiveManifest> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "image archive verification canceled by signal"
    );
    let expected_archive = normalize_sha256(expected_archive_sha256, "--sha256")?;
    let expected_os = expected_os_image_hash
        .map(|h| normalize_sha256(h, "--os-image-hash"))
        .transpose()?;

    // Verify-before-parse: hash raw downloaded bytes before touching zstd/tar.
    let actual_archive = sha256_file_streaming_cancellable(archive_path, cancel)?;
    ensure!(
        actual_archive == expected_archive,
        "archive SHA-256 mismatch: expected {expected_archive}, got {actual_archive}"
    );

    ensure!(
        dest_dir.is_dir(),
        "extraction destination is not a directory: {}",
        dest_dir.display()
    );

    let file = File::open(archive_path)
        .wrap_err_with(|| format!("open image archive {}", archive_path.display()))?;
    let decoder = zstd::Decoder::new(file).wrap_err("open image archive zstd stream")?;
    // Allow manifest + tar framing headroom above file payload cap on the raw stream while still
    // bounding every decompressed byte before tar parses or allocates.
    let max_stream_bytes = max_extracted_bytes
        .saturating_add(MAX_ARCHIVE_MANIFEST_BYTES)
        .saturating_add((max_files as u64 + 4).saturating_mul(1024));
    let budget = Rc::new(RefCell::new(ArchiveStreamBudget {
        total_read: 0,
        max_total: max_stream_bytes,
        entry_meta_read: 0,
        max_entry_meta: max_entry_meta_bytes,
        counting_meta: true,
    }));
    let bounded = BoundedArchiveReader {
        inner: decoder,
        budget: Rc::clone(&budget),
        cancel,
    };
    let mut tar = tar::Archive::new(bounded);
    let mut entries = tar.entries().wrap_err("read image archive tar entries")?;

    let mut first = entries
        .next()
        .ok_or_else(|| eyre::eyre!("image archive is empty"))?
        .wrap_err("read image archive manifest entry")?;
    ensure!(
        first.path()?.as_ref() == Path::new(ARCHIVE_MANIFEST_NAME)
            && first.header().entry_type().is_file()
            && first.size() <= MAX_ARCHIVE_MANIFEST_BYTES,
        "image archive must begin with regular {ARCHIVE_MANIFEST_NAME} (<= {MAX_ARCHIVE_MANIFEST_BYTES} bytes)"
    );
    let manifest_size = first.size();
    budget.borrow_mut().counting_meta = false;
    let mut raw_manifest = Vec::new();
    first
        .by_ref()
        .take(manifest_size.saturating_add(1))
        .read_to_end(&mut raw_manifest)?;
    {
        let mut b = budget.borrow_mut();
        b.counting_meta = true;
        b.entry_meta_read = 0;
    }
    ensure!(
        raw_manifest.len() as u64 == manifest_size
            && raw_manifest.len() as u64 <= MAX_ARCHIVE_MANIFEST_BYTES,
        "{ARCHIVE_MANIFEST_NAME} size mismatch or exceeds maximum {MAX_ARCHIVE_MANIFEST_BYTES} bytes"
    );
    drop(first);

    let manifest: ImageArchiveManifest =
        serde_json::from_slice(&raw_manifest).wrap_err("parse image-archive-manifest.json")?;
    manifest.validate()?;
    ensure!(
        manifest.files.len() <= max_files,
        "image manifest lists {} files, exceeding limit {max_files}",
        manifest.files.len()
    );

    if let Some(expected_os) = &expected_os {
        ensure!(
            manifest.os_image_hash == *expected_os,
            "archive os_image_hash mismatch: expected {expected_os}, archive declares {}",
            manifest.os_image_hash
        );
    }

    // Persist the verified archive manifest inside the image directory so offline `image verify`
    // can check every file later.
    write_new_file(&dest_dir.join(ARCHIVE_MANIFEST_NAME), &raw_manifest, 0o644)?;

    let declared: BTreeMap<&str, &ImageFileEntry> = manifest
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut found = BTreeSet::new();
    let mut total_extracted: u64 = 0;

    for entry in entries {
        let mut entry = entry.wrap_err("read image archive entry")?;
        ensure!(
            found.len() < max_files,
            "image archive exceeds maximum {max_files} files"
        );
        ensure!(
            entry.header().entry_type().is_file(),
            "image archive contains a non-file entry"
        );
        let rel = entry.path()?.into_owned();
        let rel_str = rel
            .to_str()
            .ok_or_else(|| eyre::eyre!("image archive contains a non-UTF-8 path"))?;
        safe_relative_path(rel_str)?;
        let expected = declared
            .get(rel_str)
            .ok_or_else(|| eyre::eyre!("undeclared file in image archive: {rel_str}"))?;
        ensure!(
            found.insert(rel_str.to_owned()),
            "duplicate file in image archive: {rel_str}"
        );
        ensure!(
            entry.size() == expected.size,
            "size mismatch for {rel_str}: declared {}, archive has {}",
            expected.size,
            entry.size()
        );
        let entry_mode = entry.header().mode()? & 0o7777;
        ensure!(
            entry_mode == expected.mode,
            "mode mismatch for {rel_str}: declared {:04o}, archive has {:04o}",
            expected.mode,
            entry_mode
        );

        total_extracted = total_extracted
            .checked_add(expected.size)
            .ok_or_else(|| eyre::eyre!("extracted size overflow"))?;
        ensure!(
            total_extracted <= max_extracted_bytes,
            "extracted image size exceeds maximum {max_extracted_bytes} bytes"
        );

        budget.borrow_mut().counting_meta = false;
        let actual_digest = extract_and_hash_entry(
            &mut entry,
            dest_dir,
            rel_str,
            expected.size,
            expected.mode,
            cancel,
        )?;
        {
            let mut b = budget.borrow_mut();
            b.counting_meta = true;
            b.entry_meta_read = 0;
        }
        ensure!(
            actual_digest == expected.sha256,
            "SHA-256 mismatch for {rel_str}: expected {}, got {actual_digest}",
            expected.sha256
        );
    }

    ensure!(
        found.len() == declared.len(),
        "image archive is missing declared files (extracted {}, declared {})",
        found.len(),
        declared.len()
    );

    // Cross-check extracted digest.txt, metadata.json, and sha256sum.txt.
    let extracted_digest = read_digest_txt(dest_dir)?;
    ensure!(
        extracted_digest == manifest.os_image_hash,
        "extracted digest.txt ({extracted_digest}) does not match manifest os_image_hash ({})",
        manifest.os_image_hash
    );
    verify_metadata_json(dest_dir)?;
    verify_sha256sum_txt_if_present(dest_dir)?;

    Ok(manifest)
}

/// Verify an extracted or built guest image directory against `--manifest <file>` (either
/// `vm-image-manifest.json` or `image-archive-manifest.json`) or its embedded
/// `image-archive-manifest.json`.
pub fn verify_image_dir(
    image_dir: &Path,
    manifest_path: Option<&Path>,
    flavor_hint: Option<Flavor>,
) -> Result<VerifiedImageDir> {
    let dir_meta = fs::symlink_metadata(image_dir)
        .wrap_err_with(|| format!("image directory does not exist: {}", image_dir.display()))?;
    ensure!(
        dir_meta.is_dir() && !dir_meta.file_type().is_symlink(),
        "image path is not a directory: {}",
        image_dir.display()
    );

    let actual_files = scan_image_dir_strict(image_dir)?;
    let actual_digest = read_digest_txt(image_dir)?;
    verify_metadata_json(image_dir)?;
    verify_sha256sum_txt_if_present(image_dir)?;

    let (expected_flavor, expected_hash, expected_entries, manifest_source, allow_disk_raw) =
        match manifest_path {
            Some(path) => {
                let raw = fs::read(path).wrap_err_with(|| {
                    format!("failed to read manifest file: {}", path.display())
                })?;
                let parsed: serde_json::Value = serde_json::from_slice(&raw)
                    .wrap_err_with(|| format!("invalid JSON manifest: {}", path.display()))?;
                if parsed.get("flavors").is_some() {
                    let vm_manifest: VmImageManifest = serde_json::from_value(parsed)
                        .wrap_err("invalid vm-image-manifest.json")?;
                    let (flavor, artifact) =
                        select_flavor_artifact(&vm_manifest, flavor_hint, &actual_digest)?;
                    ensure!(
                        !artifact.files.is_empty(),
                        "vm-image-manifest.json flavor '{}' has no file hashes; rebuild with S5 or pass image-archive-manifest.json",
                        flavor.as_str()
                    );
                    validate_file_entries(&artifact.files)?;
                    (
                        Some(flavor),
                        artifact.os_image_hash.clone(),
                        artifact.files.clone(),
                        path.display().to_string(),
                        true,
                    )
                } else {
                    let archive_manifest: ImageArchiveManifest = serde_json::from_value(parsed)
                        .wrap_err("invalid image-archive-manifest.json")?;
                    archive_manifest.validate()?;
                    if let Some(hint) = flavor_hint {
                        ensure!(
                            archive_manifest.flavor == hint,
                            "manifest flavor {} does not match requested --flavor {}",
                            archive_manifest.flavor.as_str(),
                            hint.as_str()
                        );
                    }
                    (
                        Some(archive_manifest.flavor),
                        archive_manifest.os_image_hash,
                        archive_manifest.files,
                        path.display().to_string(),
                        false,
                    )
                }
            }
            None => {
                let embedded = image_dir.join(ARCHIVE_MANIFEST_NAME);
                ensure!(
                    embedded.is_file(),
                    "no --manifest provided and {} is absent in {}; pass --manifest <file>",
                    ARCHIVE_MANIFEST_NAME,
                    image_dir.display()
                );
                let raw =
                    fs::read(&embedded).wrap_err_with(|| format!("read {}", embedded.display()))?;
                let archive_manifest: ImageArchiveManifest = serde_json::from_slice(&raw)
                    .wrap_err("invalid embedded image-archive-manifest.json")?;
                archive_manifest.validate()?;
                if let Some(hint) = flavor_hint {
                    ensure!(
                        archive_manifest.flavor == hint,
                        "embedded manifest flavor {} does not match requested --flavor {}",
                        archive_manifest.flavor.as_str(),
                        hint.as_str()
                    );
                }
                (
                    Some(archive_manifest.flavor),
                    archive_manifest.os_image_hash,
                    archive_manifest.files,
                    embedded.display().to_string(),
                    false,
                )
            }
        };

    ensure!(
        actual_digest == expected_hash,
        "os_image_hash mismatch: digest.txt has {actual_digest}, manifest expects {expected_hash}"
    );

    let expected_map: BTreeMap<&str, &ImageFileEntry> = expected_entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();

    // Every file on disk (other than the embedded archive manifest, and optional disk.raw in a raw
    // build output directory) must be declared in the manifest.
    for rel in actual_files.keys() {
        if rel == ARCHIVE_MANIFEST_NAME {
            continue;
        }
        if allow_disk_raw && EXCLUDED_DISTRIBUTION_FILES.contains(&rel.as_str()) {
            continue;
        }
        ensure!(
            expected_map.contains_key(rel.as_str()),
            "unexpected undeclared file in image directory: {rel}"
        );
    }

    for entry in &expected_entries {
        let actual_meta = actual_files.get(&entry.path).ok_or_else(|| {
            eyre::eyre!("missing declared file in image directory: {}", entry.path)
        })?;
        ensure!(
            actual_meta.size == entry.size,
            "size mismatch for {}: expected {}, got {}",
            entry.path,
            entry.size,
            actual_meta.size
        );
        ensure!(
            actual_meta.mode == entry.mode,
            "mode mismatch for {}: expected {:04o}, got {:04o}",
            entry.path,
            entry.mode,
            actual_meta.mode
        );
        let actual_sha = sha256_file_streaming(&image_dir.join(&entry.path))?;
        ensure!(
            actual_sha == entry.sha256,
            "SHA-256 mismatch for {}: expected {}, got {actual_sha}",
            entry.path,
            entry.sha256
        );
    }

    Ok(VerifiedImageDir {
        flavor: expected_flavor,
        os_image_hash: actual_digest,
        verified_files: expected_entries.len(),
        manifest_source,
    })
}

/// Strictly verify an installed image directory against an already-authenticated
/// [`ImageArchiveManifest`] (for example before reusing an already-published target during
/// `outbe-up image fetch`).
///
/// Refuses symlinks, non-regular files, missing/undeclared files, size/mode/SHA-256 mismatches,
/// or a missing/modified embedded `image-archive-manifest.json`.
pub fn verify_image_dir_against_archive_manifest(
    image_dir: &Path,
    expected: &ImageArchiveManifest,
    cancel: &AtomicBool,
) -> Result<VerifiedImageDir> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "image verification canceled by signal"
    );
    expected.validate()?;
    let dir_meta = fs::symlink_metadata(image_dir)
        .wrap_err_with(|| format!("image directory does not exist: {}", image_dir.display()))?;
    ensure!(
        dir_meta.is_dir() && !dir_meta.file_type().is_symlink(),
        "image path is not a directory: {}",
        image_dir.display()
    );

    let actual_files = scan_image_dir_strict(image_dir)?;
    let actual_digest = read_digest_txt(image_dir)?;
    ensure!(
        actual_digest == expected.os_image_hash,
        "installed image digest.txt ({actual_digest}) does not match expected os_image_hash ({})",
        expected.os_image_hash
    );

    let embedded_path = image_dir.join(ARCHIVE_MANIFEST_NAME);
    let embedded_meta = fs::symlink_metadata(&embedded_path).wrap_err_with(|| {
        format!(
            "installed image is missing {}: {}",
            ARCHIVE_MANIFEST_NAME,
            embedded_path.display()
        )
    })?;
    ensure!(
        embedded_meta.is_file() && !embedded_meta.file_type().is_symlink(),
        "installed {} is not a regular file: {}",
        ARCHIVE_MANIFEST_NAME,
        embedded_path.display()
    );
    let embedded_raw =
        fs::read(&embedded_path).wrap_err_with(|| format!("read {}", embedded_path.display()))?;
    let embedded_manifest: ImageArchiveManifest = serde_json::from_slice(&embedded_raw)
        .wrap_err("installed image-archive-manifest.json is corrupt")?;
    ensure!(
        embedded_manifest == *expected,
        "installed image-archive-manifest.json does not match authenticated archive manifest"
    );

    verify_metadata_json(image_dir)?;
    verify_sha256sum_txt_if_present(image_dir)?;

    let expected_map: BTreeMap<&str, &ImageFileEntry> = expected
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();

    for rel in actual_files.keys() {
        if rel == ARCHIVE_MANIFEST_NAME {
            continue;
        }
        ensure!(
            expected_map.contains_key(rel.as_str()),
            "installed image contains unexpected undeclared file: {rel}"
        );
    }

    for entry in &expected.files {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "image verification canceled by signal"
        );
        let actual_meta = actual_files.get(&entry.path).ok_or_else(|| {
            eyre::eyre!("installed image is missing declared file: {}", entry.path)
        })?;
        ensure!(
            actual_meta.size == entry.size,
            "installed image size mismatch for {}: expected {}, got {}",
            entry.path,
            entry.size,
            actual_meta.size
        );
        ensure!(
            actual_meta.mode == entry.mode,
            "installed image mode mismatch for {}: expected {:04o}, got {:04o}",
            entry.path,
            entry.mode,
            actual_meta.mode
        );
        let actual_sha = sha256_file_streaming_cancellable(&image_dir.join(&entry.path), cancel)?;
        ensure!(
            actual_sha == entry.sha256,
            "installed image SHA-256 mismatch for {}: expected {}, got {actual_sha}",
            entry.path,
            entry.sha256
        );
    }

    Ok(VerifiedImageDir {
        flavor: Some(expected.flavor),
        os_image_hash: actual_digest,
        verified_files: expected.files.len(),
        manifest_source: embedded_path.display().to_string(),
    })
}

fn select_flavor_artifact<'a>(
    manifest: &'a VmImageManifest,
    flavor_hint: Option<Flavor>,
    actual_digest: &str,
) -> Result<(Flavor, &'a crate::FlavorArtifact)> {
    if let Some(flavor) = flavor_hint {
        let artifact = manifest
            .flavors
            .get(flavor.as_str())
            .ok_or_else(|| eyre::eyre!("manifest does not contain flavor '{}'", flavor.as_str()))?;
        return Ok((flavor, artifact));
    }
    let matches: Vec<(Flavor, &crate::FlavorArtifact)> = manifest
        .flavors
        .iter()
        .filter_map(|(name, artifact)| {
            if artifact.os_image_hash == actual_digest {
                let flavor = match name.as_str() {
                    "dev" => Some(Flavor::Dev),
                    "prod" => Some(Flavor::Prod),
                    _ => None,
                }?;
                Some((flavor, artifact))
            } else {
                None
            }
        })
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    if manifest.flavors.len() == 1 {
        let (name, artifact) = manifest.flavors.iter().next().unwrap();
        let flavor = match name.as_str() {
            "dev" => Flavor::Dev,
            "prod" => Flavor::Prod,
            other => {
                return Err(eyre::eyre!(
                    "unsupported flavor '{other}' in vm-image-manifest.json"
                ));
            }
        };
        return Ok((flavor, artifact));
    }
    bail!(
        "could not uniquely select flavor from vm-image-manifest.json for digest {actual_digest}; specify --flavor dev|prod"
    );
}

pub(crate) fn collect_image_files(image_dir: &Path) -> Result<Vec<ImageFileEntry>> {
    let scanned = scan_image_dir_strict(image_dir)?;
    let mut entries = Vec::new();
    for (rel, meta) in scanned {
        if rel == ARCHIVE_MANIFEST_NAME || EXCLUDED_DISTRIBUTION_FILES.contains(&rel.as_str()) {
            continue;
        }
        let sha256 = sha256_file_streaming(&image_dir.join(&rel))?;
        entries.push(ImageFileEntry {
            path: rel,
            size: meta.size,
            sha256,
            mode: meta.mode,
        });
    }
    validate_file_entries(&entries)?;
    Ok(entries)
}

struct ScannedFileMeta {
    size: u64,
    mode: u32,
}

fn scan_image_dir_strict(image_dir: &Path) -> Result<BTreeMap<String, ScannedFileMeta>> {
    let mut files = BTreeMap::new();
    for entry in WalkDir::new(image_dir).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        if path == image_dir {
            continue;
        }
        let meta =
            fs::symlink_metadata(path).wrap_err_with(|| format!("stat {}", path.display()))?;
        let ft = meta.file_type();
        ensure!(
            !ft.is_symlink(),
            "refusing symlink in image directory: {}",
            path.display()
        );
        if ft.is_dir() {
            continue;
        }
        ensure!(
            ft.is_file(),
            "refusing non-regular file in image directory: {}",
            path.display()
        );
        let rel = path
            .strip_prefix(image_dir)?
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 path in image directory: {}", path.display()))?
            .replace('\\', "/");
        safe_relative_path(&rel)?;
        let mode = meta.permissions().mode() & 0o7777;
        ensure!(
            mode & 0o6000 == 0,
            "refusing setuid/setgid file in image directory (mode {mode:04o}): {rel}"
        );
        files.insert(
            rel,
            ScannedFileMeta {
                size: meta.len(),
                mode: mode & 0o777,
            },
        );
    }
    Ok(files)
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

fn extract_and_hash_entry(
    entry: &mut tar::Entry<'_, impl Read>,
    dest_dir: &Path,
    rel_path: &str,
    expected_size: u64,
    mode: u32,
    cancel: &AtomicBool,
) -> Result<String> {
    let target = dest_dir.join(rel_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&target)
        .wrap_err_with(|| format!("create extracted file {}", target.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "image archive extraction canceled by signal"
        );
        let n = entry.read(&mut buf)?;
        if n == 0 {
            break;
        }
        written = written
            .checked_add(n as u64)
            .ok_or_else(|| eyre::eyre!("extracted file size overflow"))?;
        ensure!(
            written <= expected_size,
            "entry {rel_path} exceeded declared size {expected_size}"
        );
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    ensure!(
        written == expected_size,
        "truncated entry {rel_path}: wrote {written} of {expected_size} bytes"
    );
    file.sync_all()?;
    fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
    Ok(hex::encode(hasher.finalize()))
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
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
mod tests {
    use super::*;

    fn write_file_mode(path: &Path, bytes: &[u8], mode: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn build_sample_image_dir(dir: &Path, digest: &str) {
        write_file_mode(
            &dir.join("digest.txt"),
            format!("{digest}\n").as_bytes(),
            0o644,
        );
        write_file_mode(&dir.join("ovmf.fd"), b"fake-bios-bytes", 0o644);
        write_file_mode(&dir.join("bzImage"), b"fake-kernel-bytes", 0o644);
        write_file_mode(&dir.join("initramfs.cpio.gz"), b"fake-initrd", 0o644);
        write_file_mode(&dir.join("rootfs.squashfs"), b"fake-rootfs-payload", 0o644);
        write_file_mode(
            &dir.join("metadata.json"),
            br#"{"bios":"ovmf.fd","kernel":"bzImage","initrd":"initramfs.cpio.gz","rootfs":"rootfs.squashfs"}"#,
            0o644,
        );
        // Build-time-only disk.raw must be excluded from the distribution archive.
        write_file_mode(&dir.join("disk.raw"), b"huge-raw-disk-excluded", 0o644);
        let rootfs_sha = sha256_file_streaming(&dir.join("rootfs.squashfs")).unwrap();
        write_file_mode(
            &dir.join("sha256sum.txt"),
            format!("{rootfs_sha}  rootfs.squashfs\n").as_bytes(),
            0o644,
        );
    }

    #[test]
    fn pack_verify_extract_round_trip_is_deterministic_and_excludes_disk_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let digest = "0123456789abcdef".repeat(4);
        let src = tmp.path().join("src-image");
        build_sample_image_dir(&src, &digest);

        let meta = ArchiveBuildMetadata {
            flavor: Flavor::Dev,
            source_commit: "a".repeat(40).leak(),
            dstack_rev: "b".repeat(40).leak(),
            overlay_commit: "c".repeat(40).leak(),
        };

        let archive_a = tmp.path().join("a.tar.zst");
        let archive_b = tmp.path().join("b.tar.zst");
        let (manifest_a, sha_a) = pack_flavor_archive(&src, &meta, &archive_a).unwrap();
        let (manifest_b, sha_b) = pack_flavor_archive(&src, &meta, &archive_b).unwrap();
        assert_eq!(manifest_a, manifest_b);
        assert_eq!(sha_a, sha_b, "archive packing must be byte-identical");
        assert!(!manifest_a.files.iter().any(|f| f.path == "disk.raw"));

        let extracted = tmp.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        let verified_manifest =
            verify_archive_and_extract(&archive_a, &sha_a, Some(&digest), &extracted).unwrap();
        assert_eq!(verified_manifest, manifest_a);
        assert!(!extracted.join("disk.raw").exists());
        assert!(extracted.join(ARCHIVE_MANIFEST_NAME).is_file());

        // Offline verify_image_dir succeeds using embedded image-archive-manifest.json.
        let verified_dir = verify_image_dir(&extracted, None, Some(Flavor::Dev)).unwrap();
        assert_eq!(verified_dir.os_image_hash, digest);
        assert_eq!(verified_dir.flavor, Some(Flavor::Dev));
        assert_eq!(verified_dir.verified_files, manifest_a.files.len());

        // Tampering with one byte of rootfs fails verify_image_dir.
        write_file_mode(
            &extracted.join("rootfs.squashfs"),
            b"tampered-rootfs",
            0o644,
        );
        let err = verify_image_dir(&extracted, None, None).expect_err("tampered file must fail");
        assert!(
            err.to_string().contains("sha256sum.txt mismatch")
                || err.to_string().contains("SHA-256 mismatch"),
            "{err}"
        );
    }

    #[test]
    fn verify_archive_rejects_hash_mismatch_before_decompression_and_caps_stream_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let garbage = tmp.path().join("corrupt.tar.zst");
        fs::write(&garbage, b"not-valid-zstd-at-all").unwrap();
        let out = tmp.path().join("out");
        fs::create_dir(&out).unwrap();

        // 1. Verify-before-parse: wrong SHA-256 fails on SHA-256 mismatch, NOT zstd decode error.
        let err = verify_archive_and_extract(&garbage, &"0".repeat(64), None, &out)
            .expect_err("wrong archive sha256 must fail before decompression");
        assert!(
            err.to_string().contains("archive SHA-256 mismatch"),
            "{err}"
        );

        // 2. Oversized PAX extended header (> 1 MiB) fails before tar allocates it even when the
        // outer archive SHA-256 matches.
        let pax_bomb = tmp.path().join("pax-bomb.tar.zst");
        {
            let file = File::create(&pax_bomb).unwrap();
            let mut enc = zstd::Encoder::new(file, 3).unwrap();
            let pax_size: u64 = 2 * 1024 * 1024;
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(tar::EntryType::XHeader);
            h.set_mode(0o644);
            h.set_size(pax_size);
            h.set_path("PaxHeaders.0/huge").unwrap();
            h.set_cksum();
            enc.write_all(h.as_bytes()).unwrap();
            let chunk = vec![b'x'; 64 * 1024];
            let mut rem = pax_size as usize;
            while rem > 0 {
                let n = rem.min(chunk.len());
                enc.write_all(&chunk[..n]).unwrap();
                rem -= n;
            }
            enc.finish().unwrap();
        }
        let pax_sha = sha256_file_streaming(&pax_bomb).unwrap();
        let pax_out = tmp.path().join("pax-out");
        fs::create_dir(&pax_out).unwrap();
        let err = verify_archive_and_extract(&pax_bomb, &pax_sha, None, &pax_out)
            .expect_err("PAX header bomb must be rejected before allocation");
        assert!(
            format!("{err:#}").contains("entry header/metadata exceeds maximum"),
            "{err:#}"
        );

        // 3. File count cap and extracted byte cap are enforced.
        let digest = "fedcba9876543210".repeat(4);
        let src = tmp.path().join("src2");
        build_sample_image_dir(&src, &digest);
        let meta = ArchiveBuildMetadata {
            flavor: Flavor::Prod,
            source_commit: "a".repeat(40).leak(),
            dstack_rev: "b".repeat(40).leak(),
            overlay_commit: "c".repeat(40).leak(),
        };
        let valid_archive = tmp.path().join("valid.tar.zst");
        let (_, valid_sha) = pack_flavor_archive(&src, &meta, &valid_archive).unwrap();

        let never = AtomicBool::new(false);
        let cap_files_out = tmp.path().join("cap-files");
        fs::create_dir(&cap_files_out).unwrap();
        let err = verify_archive_and_extract_with_limits(
            &valid_archive,
            &valid_sha,
            None,
            &cap_files_out,
            2,
            MAX_EXTRACTED_BYTES,
            MAX_ARCHIVE_HEADER_METADATA_BYTES,
            &never,
        )
        .expect_err("file count cap must be enforced");
        assert!(err.to_string().contains("exceeding limit 2"), "{err}");

        let cap_bytes_out = tmp.path().join("cap-bytes");
        fs::create_dir(&cap_bytes_out).unwrap();
        let err = verify_archive_and_extract_with_limits(
            &valid_archive,
            &valid_sha,
            None,
            &cap_bytes_out,
            MAX_ARCHIVE_FILES,
            16,
            MAX_ARCHIVE_HEADER_METADATA_BYTES,
            &never,
        )
        .expect_err("extracted byte cap must be enforced");
        assert!(
            err.to_string()
                .contains("extracted image size exceeds maximum 16"),
            "{err}"
        );
    }

    #[test]
    fn pack_refuses_existing_dangling_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let digest = "1111111111111111".repeat(4);
        let src = tmp.path().join("src");
        build_sample_image_dir(&src, &digest);
        let meta = ArchiveBuildMetadata {
            flavor: Flavor::Dev,
            source_commit: "a".repeat(40).leak(),
            dstack_rev: "b".repeat(40).leak(),
            overlay_commit: "c".repeat(40).leak(),
        };
        let dangling = tmp.path().join("dangling.tar.zst");
        std::os::unix::fs::symlink(tmp.path().join("missing-target"), &dangling).unwrap();
        let err = pack_flavor_archive(&src, &meta, &dangling)
            .expect_err("dangling symlink output must be refused");
        assert!(err.to_string().contains("already exists"), "{err}");
    }
}
