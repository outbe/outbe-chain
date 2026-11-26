//! Integrity checks of built, extracted, or installed guest image directories.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use eyre::{bail, ensure, Context, Result};
use walkdir::WalkDir;

use super::{
    read_digest_txt, safe_relative_path, sha256_file_streaming, sha256_file_streaming_cancellable,
    validate_file_entries, verify_metadata_json, verify_sha256sum_txt_if_present,
    ImageArchiveManifest, ImageFileEntry, VerifiedImageDir, ARCHIVE_MANIFEST_NAME,
    EXCLUDED_DISTRIBUTION_FILES,
};
use crate::{Flavor, FlavorArtifact, VmImageManifest};

/// Expected image contents taken from one manifest.
struct ExpectedImage {
    flavor: Option<Flavor>,
    os_image_hash: String,
    files: Vec<ImageFileEntry>,
    source: String,
    /// A raw build output directory may still hold build-only files such as `disk.raw`.
    allow_build_only_files: bool,
}

impl ExpectedImage {
    /// Take the expected contents from an image archive manifest. `manifest_kind` names the
    /// manifest in the flavor mismatch error.
    fn from_archive_manifest(
        manifest: ImageArchiveManifest,
        flavor_hint: Option<Flavor>,
        manifest_kind: &str,
        source: String,
    ) -> Result<Self> {
        manifest.validate()?;
        if let Some(hint) = flavor_hint {
            ensure!(
                manifest.flavor == hint,
                "{manifest_kind} flavor {} does not match requested --flavor {}",
                manifest.flavor.as_str(),
                hint.as_str()
            );
        }
        Ok(Self {
            flavor: Some(manifest.flavor),
            os_image_hash: manifest.os_image_hash,
            files: manifest.files,
            source,
            allow_build_only_files: false,
        })
    }

    /// Return true when the on-disk file `rel` needs no manifest entry.
    fn ignores_file(&self, rel: &str) -> bool {
        rel == ARCHIVE_MANIFEST_NAME
            || (self.allow_build_only_files && EXCLUDED_DISTRIBUTION_FILES.contains(&rel))
    }
}

/// Verify an extracted or built guest image directory against `--manifest <file>` (either
/// `vm-image-manifest.json` or `image-archive-manifest.json`) or its embedded
/// `image-archive-manifest.json`.
pub fn verify_image_dir(
    image_dir: &Path,
    manifest_path: Option<&Path>,
    flavor_hint: Option<Flavor>,
) -> Result<VerifiedImageDir> {
    ensure_real_directory(image_dir)?;

    let actual_files = scan_image_dir_strict(image_dir)?;
    let actual_digest = read_digest_txt(image_dir)?;
    verify_metadata_json(image_dir)?;
    verify_sha256sum_txt_if_present(image_dir)?;

    let expected = match manifest_path {
        Some(path) => expected_from_manifest_file(path, flavor_hint, &actual_digest)?,
        None => expected_from_embedded_manifest(image_dir, flavor_hint)?,
    };

    ensure!(
        actual_digest == expected.os_image_hash,
        "os_image_hash mismatch: digest.txt has {actual_digest}, manifest expects {}",
        expected.os_image_hash
    );

    let expected_map: BTreeMap<&str, &ImageFileEntry> = expected
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();

    // Every file on disk (other than the embedded archive manifest, and optional disk.raw in a raw
    // build output directory) must be declared in the manifest.
    for rel in actual_files.keys() {
        ensure!(
            expected.ignores_file(rel) || expected_map.contains_key(rel.as_str()),
            "unexpected undeclared file in image directory: {rel}"
        );
    }

    for entry in &expected.files {
        verify_declared_file(image_dir, &actual_files, entry)?;
    }

    Ok(VerifiedImageDir {
        flavor: expected.flavor,
        os_image_hash: actual_digest,
        verified_files: expected.files.len(),
        manifest_source: expected.source,
    })
}

/// Refuse a missing path, a symlink, or a non-directory at `image_dir`.
fn ensure_real_directory(image_dir: &Path) -> Result<()> {
    let dir_meta = fs::symlink_metadata(image_dir)
        .wrap_err_with(|| format!("image directory does not exist: {}", image_dir.display()))?;
    ensure!(
        dir_meta.is_dir() && !dir_meta.file_type().is_symlink(),
        "image path is not a directory: {}",
        image_dir.display()
    );
    Ok(())
}

/// Load the expected contents from `--manifest <file>`, which is either a
/// `vm-image-manifest.json` (it has `flavors`) or an `image-archive-manifest.json`.
fn expected_from_manifest_file(
    path: &Path,
    flavor_hint: Option<Flavor>,
    actual_digest: &str,
) -> Result<ExpectedImage> {
    let raw = fs::read(path)
        .wrap_err_with(|| format!("failed to read manifest file: {}", path.display()))?;
    let parsed: serde_json::Value = serde_json::from_slice(&raw)
        .wrap_err_with(|| format!("invalid JSON manifest: {}", path.display()))?;
    if parsed.get("flavors").is_none() {
        let archive_manifest: ImageArchiveManifest =
            serde_json::from_value(parsed).wrap_err("invalid image-archive-manifest.json")?;
        return ExpectedImage::from_archive_manifest(
            archive_manifest,
            flavor_hint,
            "manifest",
            path.display().to_string(),
        );
    }
    let vm_manifest: VmImageManifest =
        serde_json::from_value(parsed).wrap_err("invalid vm-image-manifest.json")?;
    let (flavor, artifact) = select_flavor_artifact(&vm_manifest, flavor_hint, actual_digest)?;
    ensure!(
        !artifact.files.is_empty(),
        "vm-image-manifest.json flavor '{}' has no file hashes; rebuild with S5 or pass image-archive-manifest.json",
        flavor.as_str()
    );
    validate_file_entries(&artifact.files)?;
    Ok(ExpectedImage {
        flavor: Some(flavor),
        os_image_hash: artifact.os_image_hash.clone(),
        files: artifact.files.clone(),
        source: path.display().to_string(),
        allow_build_only_files: true,
    })
}

/// Load the expected contents from the `image-archive-manifest.json` embedded in `image_dir`.
fn expected_from_embedded_manifest(
    image_dir: &Path,
    flavor_hint: Option<Flavor>,
) -> Result<ExpectedImage> {
    let embedded = image_dir.join(ARCHIVE_MANIFEST_NAME);
    ensure!(
        embedded.is_file(),
        "no --manifest provided and {} is absent in {}; pass --manifest <file>",
        ARCHIVE_MANIFEST_NAME,
        image_dir.display()
    );
    let raw = fs::read(&embedded).wrap_err_with(|| format!("read {}", embedded.display()))?;
    let archive_manifest: ImageArchiveManifest =
        serde_json::from_slice(&raw).wrap_err("invalid embedded image-archive-manifest.json")?;
    ExpectedImage::from_archive_manifest(
        archive_manifest,
        flavor_hint,
        "embedded manifest",
        embedded.display().to_string(),
    )
}

/// Check one declared file against the scanned directory: presence, size, mode, and SHA-256.
fn verify_declared_file(
    image_dir: &Path,
    actual_files: &BTreeMap<String, ScannedFileMeta>,
    entry: &ImageFileEntry,
) -> Result<()> {
    let actual_meta = actual_files
        .get(&entry.path)
        .ok_or_else(|| eyre::eyre!("missing declared file in image directory: {}", entry.path))?;
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
    Ok(())
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
    ensure_real_directory(image_dir)?;

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

/// Map a `vm-image-manifest.json` flavor key to a [`Flavor`].
fn flavor_from_name(name: &str) -> Option<Flavor> {
    match name {
        "dev" => Some(Flavor::Dev),
        "prod" => Some(Flavor::Prod),
        _ => None,
    }
}

fn select_flavor_artifact<'a>(
    manifest: &'a VmImageManifest,
    flavor_hint: Option<Flavor>,
    actual_digest: &str,
) -> Result<(Flavor, &'a FlavorArtifact)> {
    if let Some(flavor) = flavor_hint {
        let artifact = manifest
            .flavors
            .get(flavor.as_str())
            .ok_or_else(|| eyre::eyre!("manifest does not contain flavor '{}'", flavor.as_str()))?;
        return Ok((flavor, artifact));
    }
    let matches: Vec<(Flavor, &FlavorArtifact)> = manifest
        .flavors
        .iter()
        .filter(|(_, artifact)| artifact.os_image_hash == actual_digest)
        .filter_map(|(name, artifact)| Some((flavor_from_name(name)?, artifact)))
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    if manifest.flavors.len() == 1 {
        let (name, artifact) = manifest.flavors.iter().next().unwrap();
        let flavor = flavor_from_name(name)
            .ok_or_else(|| eyre::eyre!("unsupported flavor '{name}' in vm-image-manifest.json"))?;
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
