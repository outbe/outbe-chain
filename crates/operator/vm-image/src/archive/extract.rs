//! Verified archive extraction: hash the raw archive bytes first, then extract every declared
//! file under hard caps on file count, header metadata, decompressed stream bytes, and extracted
//! bytes.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use eyre::{ensure, Context, Result};
use rustix::fs::{openat, Mode, OFlags, CWD};
use sha2::{Digest, Sha256};

use super::{
    normalize_sha256, read_digest_txt, safe_relative_path, verify_metadata_json,
    verify_sha256sum_txt_if_present, ImageArchiveManifest, ImageFileEntry, ARCHIVE_MANIFEST_NAME,
    MAX_ARCHIVE_FILES, MAX_ARCHIVE_HEADER_METADATA_BYTES, MAX_ARCHIVE_MANIFEST_BYTES,
    MAX_EXTRACTED_BYTES,
};

/// Digests that authenticate one distribution archive before extraction.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveExpectation<'a> {
    /// Expected SHA-256 of the raw `.tar.zst` bytes.
    pub archive_sha256: &'a str,
    /// Expected `os_image_hash` that the archive manifest must declare, if known.
    pub os_image_hash: Option<&'a str>,
}

/// Hard caps for one archive extraction.
#[derive(Clone, Copy, Debug)]
pub(super) struct ExtractLimits {
    pub(super) max_files: usize,
    pub(super) max_extracted_bytes: u64,
    pub(super) max_entry_meta_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_files: MAX_ARCHIVE_FILES,
            max_extracted_bytes: MAX_EXTRACTED_BYTES,
            max_entry_meta_bytes: MAX_ARCHIVE_HEADER_METADATA_BYTES,
        }
    }
}

/// One verified extraction: the trusted digests, the destination, the caps, and the cancel flag.
pub(super) struct ExtractJob<'a> {
    pub(super) expected: ArchiveExpectation<'a>,
    pub(super) dest_dir: &'a Path,
    pub(super) limits: ExtractLimits,
    pub(super) cancel: &'a AtomicBool,
}

struct ArchiveStreamBudget {
    total_read: u64,
    max_total: u64,
    entry_meta_read: u64,
    max_entry_meta: u64,
    counting_meta: bool,
}

impl ArchiveStreamBudget {
    fn for_limits(limits: ExtractLimits) -> Self {
        // Allow manifest + tar framing headroom above file payload cap on the raw stream while
        // still bounding every decompressed byte before tar parses or allocates.
        let max_stream_bytes = limits
            .max_extracted_bytes
            .saturating_add(MAX_ARCHIVE_MANIFEST_BYTES)
            .saturating_add((limits.max_files as u64 + 4).saturating_mul(1024));
        Self {
            total_read: 0,
            max_total: max_stream_bytes,
            entry_meta_read: 0,
            max_entry_meta: limits.max_entry_meta_bytes,
            counting_meta: true,
        }
    }
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

/// Read one entry payload. Payload bytes count only toward the stream total, and the metadata
/// counter starts again at zero for the next entry header.
fn read_payload<T>(
    budget: &RefCell<ArchiveStreamBudget>,
    read: impl FnOnce() -> Result<T>,
) -> Result<T> {
    budget.borrow_mut().counting_meta = false;
    let value = read()?;
    let mut b = budget.borrow_mut();
    b.counting_meta = true;
    b.entry_meta_read = 0;
    Ok(value)
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
    let job = ExtractJob {
        expected: ArchiveExpectation {
            archive_sha256: expected_archive_sha256,
            os_image_hash: expected_os_image_hash,
        },
        dest_dir,
        limits: ExtractLimits::default(),
        cancel,
    };
    verify_archive_path_and_extract(archive_path, &job, || Ok(()))
}

/// Open a local archive path ONCE with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`, refusing symlinks,
/// non-regular files (including FIFOs without hanging), and files exceeding [`MAX_EXTRACTED_BYTES`].
fn open_regular_archive_nofollow(path: &Path) -> Result<(File, fs::Metadata)> {
    let fd = openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|err| {
        if err == rustix::io::Errno::LOOP {
            eyre::eyre!("refusing symlink image archive: {}", path.display())
        } else {
            eyre::Report::new(err).wrap_err(format!("open image archive {}", path.display()))
        }
    })?;
    let file = File::from(fd);
    let meta = file
        .metadata()
        .wrap_err_with(|| format!("fstat image archive {}", path.display()))?;
    ensure!(
        meta.file_type().is_file(),
        "refusing non-regular image archive: {}",
        path.display()
    );
    ensure!(
        meta.len() <= MAX_EXTRACTED_BYTES,
        "image archive ({} bytes) exceeds maximum {MAX_EXTRACTED_BYTES} bytes",
        meta.len()
    );
    Ok((file, meta))
}

fn sha256_reader_streaming_cancellable<R: Read>(
    reader: &mut R,
    label: &str,
    cancel: &AtomicBool,
    max_bytes: Option<u64>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "image verification canceled by signal"
        );
        let n = reader
            .read(&mut buf)
            .wrap_err_with(|| format!("read {label} for SHA-256"))?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .ok_or_else(|| eyre::eyre!("size overflow hashing {label}"))?;
        if let Some(limit) = max_bytes {
            ensure!(
                total <= limit,
                "image archive ({total} bytes) exceeds maximum {limit} bytes"
            );
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Verify and extract from an ALREADY-OPEN regular [`File`] descriptor without reopening by path
/// (eliminating TOCTOU between download/hash and extraction): `fstat` -> `seek(0)` -> SHA-256 ->
/// `seek(0)` -> zstd/tar extraction on the exact same open FD.
pub fn verify_archive_file_and_extract_cancellable(
    file: &mut File,
    label: &str,
    expected: ArchiveExpectation<'_>,
    dest_dir: &Path,
    cancel: &AtomicBool,
) -> Result<ImageArchiveManifest> {
    let job = ExtractJob {
        expected,
        dest_dir,
        limits: ExtractLimits::default(),
        cancel,
    };
    verify_archive_fd_and_extract(file, label, &job, || Ok(()))
}

/// Open `archive_path` once and run [`verify_archive_fd_and_extract`] on that descriptor.
/// `after_hash_hook` runs after the hash check and before extraction.
pub(super) fn verify_archive_path_and_extract<F>(
    archive_path: &Path,
    job: &ExtractJob<'_>,
    after_hash_hook: F,
) -> Result<ImageArchiveManifest>
where
    F: FnOnce() -> Result<()>,
{
    ensure!(
        !job.cancel.load(Ordering::Relaxed),
        "image archive verification canceled by signal"
    );
    // Open ONCE with O_NOFOLLOW | O_NONBLOCK and reuse the exact same FD for fstat, SHA-256, and
    // extraction so swapping the path between phases cannot bypass verification.
    let (mut file, _) = open_regular_archive_nofollow(archive_path)?;
    let label = archive_path.display().to_string();
    verify_archive_fd_and_extract(&mut file, &label, job, after_hash_hook)
}

fn verify_archive_fd_and_extract<F>(
    file: &mut File,
    label: &str,
    job: &ExtractJob<'_>,
    after_hash_hook: F,
) -> Result<ImageArchiveManifest>
where
    F: FnOnce() -> Result<()>,
{
    ensure!(
        !job.cancel.load(Ordering::Relaxed),
        "image archive verification canceled by signal"
    );
    let expected_archive = normalize_sha256(job.expected.archive_sha256, "--sha256")?;
    let expected_os = job
        .expected
        .os_image_hash
        .map(|h| normalize_sha256(h, "--os-image-hash"))
        .transpose()?;

    verify_archive_fd_digest(file, label, &expected_archive, job.cancel)?;

    after_hash_hook()?;

    ensure!(
        job.dest_dir.is_dir(),
        "extraction destination is not a directory: {}",
        job.dest_dir.display()
    );

    // Rewind the EXACT SAME open FD (never reopen by path) and extract from it.
    file.seek(SeekFrom::Start(0))
        .wrap_err_with(|| format!("rewind image archive {label} before extraction"))?;
    let decoder = zstd::Decoder::new(&mut *file).wrap_err("open image archive zstd stream")?;
    let budget = Rc::new(RefCell::new(ArchiveStreamBudget::for_limits(job.limits)));
    let bounded = BoundedArchiveReader {
        inner: decoder,
        budget: Rc::clone(&budget),
        cancel: job.cancel,
    };
    let mut tar = tar::Archive::new(bounded);
    let mut entries = tar.entries().wrap_err("read image archive tar entries")?;

    let (manifest, raw_manifest) = read_archive_manifest(&mut entries, &budget)?;
    check_archive_manifest(&manifest, job.limits.max_files, expected_os.as_deref())?;

    // Persist the verified archive manifest inside the image directory so offline `image verify`
    // can check every file later.
    write_new_file(
        &job.dest_dir.join(ARCHIVE_MANIFEST_NAME),
        &raw_manifest,
        0o644,
    )?;

    extract_declared_entries(entries, &manifest, job, &budget)?;
    cross_check_extracted_image(job.dest_dir, &manifest)?;

    Ok(manifest)
}

/// Check that the open archive is a regular file within the size cap, then hash its raw bytes
/// from offset zero and compare them with `expected_archive` (verify before parse).
fn verify_archive_fd_digest(
    file: &mut File,
    label: &str,
    expected_archive: &str,
    cancel: &AtomicBool,
) -> Result<()> {
    let meta = file
        .metadata()
        .wrap_err_with(|| format!("fstat image archive {label}"))?;
    ensure!(
        meta.file_type().is_file(),
        "refusing non-regular image archive: {label}"
    );
    ensure!(
        meta.len() <= MAX_EXTRACTED_BYTES,
        "image archive ({} bytes) exceeds maximum {MAX_EXTRACTED_BYTES} bytes",
        meta.len()
    );

    // Verify-before-parse: hash raw bytes from this open FD before touching zstd/tar.
    file.seek(SeekFrom::Start(0))
        .wrap_err_with(|| format!("rewind image archive {label} before hashing"))?;
    let actual_archive =
        sha256_reader_streaming_cancellable(file, label, cancel, Some(MAX_EXTRACTED_BYTES))?;
    ensure!(
        actual_archive == expected_archive,
        "archive SHA-256 mismatch: expected {expected_archive}, got {actual_archive}"
    );
    Ok(())
}

/// Read and parse the first tar entry, which must be the regular `image-archive-manifest.json`.
/// Return the parsed manifest and its raw bytes.
fn read_archive_manifest<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    budget: &RefCell<ArchiveStreamBudget>,
) -> Result<(ImageArchiveManifest, Vec<u8>)> {
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
    let raw_manifest = read_payload(budget, || {
        let mut raw = Vec::new();
        first
            .by_ref()
            .take(manifest_size.saturating_add(1))
            .read_to_end(&mut raw)?;
        Ok(raw)
    })?;
    ensure!(
        raw_manifest.len() as u64 == manifest_size
            && raw_manifest.len() as u64 <= MAX_ARCHIVE_MANIFEST_BYTES,
        "{ARCHIVE_MANIFEST_NAME} size mismatch or exceeds maximum {MAX_ARCHIVE_MANIFEST_BYTES} bytes"
    );
    drop(first);

    let manifest: ImageArchiveManifest =
        serde_json::from_slice(&raw_manifest).wrap_err("parse image-archive-manifest.json")?;
    Ok((manifest, raw_manifest))
}

/// Validate the archive manifest, its file count cap, and the expected `os_image_hash`.
fn check_archive_manifest(
    manifest: &ImageArchiveManifest,
    max_files: usize,
    expected_os: Option<&str>,
) -> Result<()> {
    manifest.validate()?;
    ensure!(
        manifest.files.len() <= max_files,
        "image manifest lists {} files, exceeding limit {max_files}",
        manifest.files.len()
    );
    if let Some(expected_os) = expected_os {
        ensure!(
            manifest.os_image_hash == expected_os,
            "archive os_image_hash mismatch: expected {expected_os}, archive declares {}",
            manifest.os_image_hash
        );
    }
    Ok(())
}

/// Extract every remaining tar entry. Each entry must match one declared manifest file exactly
/// once, and together they must cover every declared file.
fn extract_declared_entries<R: Read>(
    entries: tar::Entries<'_, R>,
    manifest: &ImageArchiveManifest,
    job: &ExtractJob<'_>,
    budget: &RefCell<ArchiveStreamBudget>,
) -> Result<()> {
    let max_extracted_bytes = job.limits.max_extracted_bytes;
    let declared: BTreeMap<&str, &ImageFileEntry> = manifest
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut found = BTreeSet::new();
    let mut total_extracted: u64 = 0;

    for entry in entries {
        let mut entry = entry.wrap_err("read image archive entry")?;
        let expected = match_declared_entry(&entry, &declared, &mut found, job.limits.max_files)?;

        total_extracted = total_extracted
            .checked_add(expected.size)
            .ok_or_else(|| eyre::eyre!("extracted size overflow"))?;
        ensure!(
            total_extracted <= max_extracted_bytes,
            "extracted image size exceeds maximum {max_extracted_bytes} bytes"
        );

        let actual_digest = read_payload(budget, || {
            extract_and_hash_entry(&mut entry, job.dest_dir, expected, job.cancel)
        })?;
        ensure!(
            actual_digest == expected.sha256,
            "SHA-256 mismatch for {}: expected {}, got {actual_digest}",
            expected.path,
            expected.sha256
        );
    }

    ensure!(
        found.len() == declared.len(),
        "image archive is missing declared files (extracted {}, declared {})",
        found.len(),
        declared.len()
    );
    Ok(())
}

/// Match one tar entry header against the declared manifest files. Refuse extra, non-file,
/// unsafe, undeclared, or duplicate entries and size or mode mismatches.
fn match_declared_entry<'m, R: Read>(
    entry: &tar::Entry<'_, R>,
    declared: &BTreeMap<&str, &'m ImageFileEntry>,
    found: &mut BTreeSet<String>,
    max_files: usize,
) -> Result<&'m ImageFileEntry> {
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
    let expected = *declared
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
    Ok(expected)
}

/// Cross-check extracted `digest.txt`, `metadata.json`, and `sha256sum.txt`.
fn cross_check_extracted_image(dest_dir: &Path, manifest: &ImageArchiveManifest) -> Result<()> {
    let extracted_digest = read_digest_txt(dest_dir)?;
    ensure!(
        extracted_digest == manifest.os_image_hash,
        "extracted digest.txt ({extracted_digest}) does not match manifest os_image_hash ({})",
        manifest.os_image_hash
    );
    verify_metadata_json(dest_dir)?;
    verify_sha256sum_txt_if_present(dest_dir)
}

/// Write one declared entry to `dest_dir/<expected.path>` with its declared mode and return the
/// SHA-256 of the written bytes.
fn extract_and_hash_entry(
    entry: &mut tar::Entry<'_, impl Read>,
    dest_dir: &Path,
    expected: &ImageFileEntry,
    cancel: &AtomicBool,
) -> Result<String> {
    let rel_path = expected.path.as_str();
    let expected_size = expected.size;
    let mode = expected.mode;
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
