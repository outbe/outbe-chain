//! Guest OS image validation, digest-qualified naming, and atomic host publication helpers.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use rustix::fs::{flock, openat, FlockOperation, Mode, OFlags, CWD};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{
    dnsmasq::{MISSING_SENTINEL, PRESENT_SENTINEL},
    shell_quote,
    vmm::{remote_vmm_path, StatusRequest, VmmClient, VmmTransport},
};

const STATUS_PAGE_SIZE: u32 = 100;
const MAX_STATUS_PAGES: u32 = 100;
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);
const ARCHIVE_MANIFEST_NAME: &str = "image-archive-manifest.json";
const ARCHIVE_MANIFEST_VERSION: u32 = 1;
const MAX_ARCHIVE_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_DECLARED_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedImageInfo {
    pub name: String,
    pub flavor: String,
    pub os_image_hash: String,
    pub path: PathBuf,
    pub has_archive_manifest: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RemoteImageAction {
    Upload,
    Reuse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedImage {
    pub name: String,
    pub digest: String,
    pub target_dir: PathBuf,
    pub reused: bool,
}

/// Exclusive `flock` guard for `<vmm_home>/image/.<name>.lock`.
///
/// Released automatically when dropped.
#[derive(Debug)]
pub struct ImageLockGuard {
    _file: fs::File,
}

/// Result of publishing (or reusing) a local guest OS image while keeping its exclusive per-image
/// lock held so callers such as `vm create` can complete VMM registration atomically with respect
/// to concurrent `image remove`.
#[derive(Debug)]
pub struct PublishedImageWithLock {
    pub published: PublishedImage,
    pub lock: ImageLockGuard,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedArchiveManifest {
    version: u32,
    flavor: String,
    os_image_hash: String,
    source_commit: String,
    dstack_rev: String,
    overlay_commit: String,
    files: Vec<ManagedArchiveFileEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedArchiveFileEntry {
    path: String,
    size: u64,
    sha256: String,
    mode: u32,
}

/// Validate a local guest OS image directory and derive its digest-qualified VMM image name.
pub fn validate_image_dir(
    image_dir: &Path,
    image_name: Option<&str>,
    flavor_label: &str,
) -> Result<(String, String)> {
    ensure!(
        image_dir.is_dir(),
        "guest image directory does not exist: {}",
        image_dir.display()
    );
    let metadata_path = image_dir.join("metadata.json");
    ensure!(
        metadata_path.is_file(),
        "guest image directory is missing metadata.json: {}",
        metadata_path.display()
    );
    let digest_path = image_dir.join("digest.txt");
    ensure!(
        digest_path.is_file(),
        "guest image directory is missing digest.txt: {}",
        digest_path.display()
    );
    let digest = fs::read_to_string(&digest_path)
        .wrap_err_with(|| format!("read {}", digest_path.display()))?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        digest.len() >= 16 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "digest.txt must contain at least 16 hex characters"
    );

    let name = match image_name {
        Some(explicit) => explicit.to_owned(),
        None => format!("outbe-vm-{flavor_label}-{}", &digest[..16]),
    };
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "invalid guest image name: {name}"
    );
    Ok((name, digest))
}

/// Decide whether to reuse or upload a guest image on a host, refusing digest mismatches.
pub fn decide_remote_image_action(
    remote_state: Option<&str>,
    local_digest: &str,
    image_name: &str,
) -> Result<RemoteImageAction> {
    match remote_state {
        None => Ok(RemoteImageAction::Upload),
        Some(remote_digest) => {
            let trimmed = remote_digest.trim().to_ascii_lowercase();
            if trimmed == local_digest {
                Ok(RemoteImageAction::Reuse)
            } else {
                bail!(
                    "refusing to replace existing guest image `{image_name}`: remote digest `{trimmed}` does not match local `{local_digest}`"
                );
            }
        }
    }
}

/// Atomically publish a validated local guest OS image directory into `<vmm_home>/image/<name>`.
///
/// Acquires an exclusive `flock` on `<vmm_home>/image/.<name>.lock`, reuses an existing image when
/// its `digest.txt` matches, and refuses to overwrite an existing image with a different digest.
pub fn publish_local_image(
    vmm_home: &Path,
    image_dir: &Path,
    image_name: Option<&str>,
    flavor_label: &str,
) -> Result<PublishedImage> {
    let res = publish_local_image_with_options::<fn(&Path) -> Result<()>>(
        vmm_home,
        image_dir,
        image_name,
        flavor_label,
        None,
        None,
        None,
    )?;
    Ok(res.published)
}

/// Same as [`publish_local_image`], honoring a cooperative `cancel` flag throughout lock wait,
/// staging copy, and right before the atomic publish rename, and running `verify_existing` under
/// the exclusive image lock before reusing an already-installed target directory.
pub fn publish_local_image_verified_cancellable<F>(
    vmm_home: &Path,
    image_dir: &Path,
    image_name: Option<&str>,
    flavor_label: &str,
    cancel: &AtomicBool,
    verify_existing: F,
) -> Result<PublishedImage>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let res = publish_local_image_with_options(
        vmm_home,
        image_dir,
        image_name,
        flavor_label,
        None,
        Some(cancel),
        Some(verify_existing),
    )?;
    Ok(res.published)
}

/// Same as [`publish_local_image`], honoring optional deadline and cancellation, and returning the
/// held [`ImageLockGuard`] so callers can register a VM on VMM before releasing the image lock.
pub fn publish_local_image_locked_cancellable(
    vmm_home: &Path,
    image_dir: &Path,
    image_name: Option<&str>,
    flavor_label: &str,
    deadline: Option<Instant>,
    cancel: Option<&AtomicBool>,
) -> Result<PublishedImageWithLock> {
    publish_local_image_with_options::<fn(&Path) -> Result<()>>(
        vmm_home,
        image_dir,
        image_name,
        flavor_label,
        deadline,
        cancel,
        None,
    )
}

fn check_image_canceled(cancel: Option<&AtomicBool>, stage: &str) -> Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        bail!("image operation canceled by signal while {stage}");
    }
    Ok(())
}

fn acquire_image_lock(
    image_root: &Path,
    name: &str,
    deadline: Option<Instant>,
    cancel: Option<&AtomicBool>,
) -> Result<ImageLockGuard> {
    check_image_canceled(cancel, "acquiring image lock")?;
    let lock_path = image_root.join(format!(".{name}.lock"));
    let current_uid = rustix::process::getuid().as_raw();
    if let Ok(meta) = fs::symlink_metadata(&lock_path) {
        ensure!(
            meta.file_type().is_file() && meta.uid() == current_uid,
            "image lock must be a regular file owned by current uid: {}",
            lock_path.display()
        );
    }
    let lock_fd = openat(
        CWD,
        &lock_path,
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .wrap_err_with(|| format!("open image lock {}", lock_path.display()))?;
    let lock_file = fs::File::from(lock_fd);
    let lock_meta = lock_file
        .metadata()
        .wrap_err_with(|| format!("stat image lock {}", lock_path.display()))?;
    ensure!(
        lock_meta.file_type().is_file() && lock_meta.uid() == current_uid,
        "image lock must be a regular file owned by current uid: {}",
        lock_path.display()
    );

    if deadline.is_none() && cancel.is_none() {
        flock(&lock_file, FlockOperation::LockExclusive)
            .wrap_err_with(|| format!("lock {}", lock_path.display()))?;
        return Ok(ImageLockGuard { _file: lock_file });
    }

    loop {
        check_image_canceled(cancel, "waiting for image lock")?;
        match flock(&lock_file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(ImageLockGuard { _file: lock_file }),
            Err(err) if err == rustix::io::Errno::WOULDBLOCK || err == rustix::io::Errno::AGAIN => {
                if let Some(dl) = deadline {
                    let now = Instant::now();
                    if now >= dl {
                        bail!(
                            "timed out waiting for exclusive image lock {}",
                            lock_path.display()
                        );
                    }
                    thread::sleep(LOCK_POLL_INTERVAL.min(dl.saturating_duration_since(now)));
                } else {
                    thread::sleep(LOCK_POLL_INTERVAL);
                }
            }
            Err(err) => {
                return Err(err).wrap_err_with(|| format!("lock {}", lock_path.display()));
            }
        }
    }
}

fn publish_local_image_with_options<F>(
    vmm_home: &Path,
    image_dir: &Path,
    image_name: Option<&str>,
    flavor_label: &str,
    deadline: Option<Instant>,
    cancel: Option<&AtomicBool>,
    verify_existing: Option<F>,
) -> Result<PublishedImageWithLock>
where
    F: FnOnce(&Path) -> Result<()>,
{
    check_image_canceled(cancel, "validating image directory")?;
    let (name, digest) = validate_image_dir(image_dir, image_name, flavor_label)?;
    let image_root = vmm_home.join("image");
    if let Ok(meta) = fs::symlink_metadata(&image_root) {
        ensure!(
            meta.file_type().is_dir(),
            "VMM image directory is not a real directory: {}",
            image_root.display()
        );
    } else {
        fs::create_dir_all(&image_root)
            .wrap_err_with(|| format!("create VMM image directory {}", image_root.display()))?;
    }

    let lock = acquire_image_lock(&image_root, &name, deadline, cancel)?;
    check_image_canceled(cancel, "checking existing image")?;

    let target_dir = image_root.join(&name);
    if let Ok(meta) = fs::symlink_metadata(&target_dir) {
        ensure!(
            meta.file_type().is_dir(),
            "existing image path is not a directory: {}",
            target_dir.display()
        );
        let existing_digest_path = target_dir.join("digest.txt");
        ensure!(
            existing_digest_path.is_file(),
            "existing image is missing digest.txt: {}",
            existing_digest_path.display()
        );
        let existing_digest = fs::read_to_string(&existing_digest_path)
            .wrap_err_with(|| format!("read {}", existing_digest_path.display()))?;
        let action = decide_remote_image_action(Some(&existing_digest), &digest, &name)?;
        debug_assert_eq!(action, RemoteImageAction::Reuse);
        if let Some(verifier) = verify_existing {
            verifier(&target_dir).wrap_err_with(|| {
                format!(
                    "refusing to reuse corrupted or unverified installed image `{name}` at {}",
                    target_dir.display()
                )
            })?;
        }
        check_image_canceled(cancel, "verifying existing image")?;
        return Ok(PublishedImageWithLock {
            published: PublishedImage {
                name,
                digest,
                target_dir,
                reused: true,
            },
            lock,
        });
    }

    // Stage into a unique temporary sibling directory so publication is one atomic rename.
    let stage = tempfile::Builder::new()
        .prefix(&format!(".{name}.tmp."))
        .tempdir_in(&image_root)
        .wrap_err_with(|| format!("create staging directory in {}", image_root.display()))?;
    copy_image_tree_cancellable(image_dir, stage.path(), cancel)?;

    let staged_digest = fs::read_to_string(stage.path().join("digest.txt"))
        .wrap_err("verify staged image digest.txt")?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        staged_digest == digest,
        "staged image digest mismatch: {staged_digest} != {digest}"
    );

    // Final cancellation barrier right before the atomic publish rename so a late signal aborts
    // cleanly without publishing (dropping `stage` removes the temporary staging directory).
    check_image_canceled(cancel, "publishing staged image")?;

    let staged_path = stage.keep();
    if let Err(err) = fs::rename(&staged_path, &target_dir) {
        let _ = fs::remove_dir_all(&staged_path);
        return Err(err).wrap_err_with(|| {
            format!(
                "publish guest image {} to {}",
                staged_path.display(),
                target_dir.display()
            )
        });
    }
    if let Ok(parent_dir) = fs::File::open(&image_root) {
        let _ = parent_dir.sync_all();
    }

    let published_digest = fs::read_to_string(target_dir.join("digest.txt"))
        .wrap_err("read published image digest.txt")?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        published_digest == digest,
        "published image digest mismatch: {published_digest} != {digest}"
    );

    Ok(PublishedImageWithLock {
        published: PublishedImage {
            name,
            digest,
            target_dir,
            reused: false,
        },
        lock,
    })
}

/// List all Outbe-managed guest OS images stored under `<vmm_home>/image/`.
///
/// Only images proven to be managed by `outbe-up` (digest-qualified name
/// `outbe-vm-(dev|prod)-<hash16>`, strict v1 `image-archive-manifest.json` matching the directory
/// flavor and hash prefix, and matching `digest.txt`) are returned; foreign or unverified
/// directories are omitted.
pub fn list_local_images(vmm_home: &Path) -> Result<Vec<ManagedImageInfo>> {
    let image_root = vmm_home.join("image");
    let entries = match fs::read_dir(&image_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err)
                .wrap_err_with(|| format!("read VMM image directory {}", image_root.display()));
        }
    };

    let mut images = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name_os = entry.file_name();
        let Some(name) = name_os.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if let Ok(info) = verify_managed_image_provenance(&path, name) {
            images.push(info);
        }
    }

    images.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(images)
}

/// Remove an Outbe-managed local guest OS image from `<vmm_home>/image/<image_name>` only after
/// verifying its `outbe-up` provenance and confirming via VMM `Status` under the per-image lock
/// that zero VMs (running, stopped, exited, or removing) reference it.
///
/// Fails closed if the image is not proven to be ours or if the VMM socket is unreachable or
/// returns an error.
pub fn remove_local_image<T: VmmTransport>(
    vmm_home: &Path,
    client: &VmmClient<T>,
    image_name: &str,
) -> Result<PathBuf> {
    // Reject non-outbe-up names before creating any lock file in `<vmm_home>/image/`.
    parse_managed_image_name(image_name)?;
    let image_root = vmm_home.join("image");
    let root_meta = fs::symlink_metadata(&image_root).wrap_err_with(|| {
        format!(
            "VMM image directory does not exist: {}",
            image_root.display()
        )
    })?;
    ensure!(
        root_meta.is_dir() && !root_meta.file_type().is_symlink(),
        "VMM image directory is not a real directory: {}",
        image_root.display()
    );

    let lock = acquire_image_lock(&image_root, image_name, None, None)?;

    let target_dir = image_root.join(image_name);
    let target_meta = fs::symlink_metadata(&target_dir).wrap_err_with(|| {
        format!(
            "guest image `{image_name}` does not exist in {}",
            image_root.display()
        )
    })?;
    ensure!(
        target_meta.is_dir() && !target_meta.file_type().is_symlink(),
        "refusing to remove non-directory or symlink image path: {}",
        target_dir.display()
    );

    verify_managed_image_provenance(&target_dir, image_name).wrap_err_with(|| {
        format!("refusing to remove `{image_name}`: not a verified outbe-up managed image")
    })?;

    // Fail closed: enumerate every VM known to VMM under the image lock and refuse removal if any
    // VM references this image.
    let vms = list_all_vmm_vms(client).wrap_err(
        "cannot remove guest image: failed to query VMM for active/stopped VM references",
    )?;
    let referencing: Vec<String> = vms
        .iter()
        .filter(|vm| vm.image_name() == Some(image_name))
        .map(|vm| format!("{} ({}, status={})", vm.name, vm.id, vm.status))
        .collect();
    ensure!(
        referencing.is_empty(),
        "refusing to remove guest image `{image_name}`: still referenced by VM(s): {}",
        referencing.join(", ")
    );

    // Atomically move aside under the lock so no reader observes a half-deleted image tree.
    let trash = tempfile::Builder::new()
        .prefix(&format!(".removing-{image_name}."))
        .tempdir_in(&image_root)
        .wrap_err_with(|| {
            format!(
                "create removal staging directory in {}",
                image_root.display()
            )
        })?;
    let trash_dir = trash.path().join("image");
    fs::rename(&target_dir, &trash_dir).wrap_err_with(|| {
        format!(
            "move guest image {} aside before deletion",
            target_dir.display()
        )
    })?;
    if let Ok(parent_dir) = fs::File::open(&image_root) {
        let _ = parent_dir.sync_all();
    }
    trash
        .close()
        .wrap_err_with(|| format!("delete guest image staging directory for `{image_name}`"))?;

    drop(lock);
    Ok(target_dir)
}

fn is_lower_hex(s: &str, expected_len: usize) -> bool {
    s.len() == expected_len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn parse_managed_image_name(name: &str) -> Result<(&str, &str)> {
    let Some(rest) = name.strip_prefix("outbe-vm-") else {
        bail!(
            "refusing unmanaged image name `{name}`: expected `outbe-vm-(dev|prod)-<16-lowercase-hex>`"
        );
    };
    let Some((flavor, hash16)) = rest.split_once('-') else {
        bail!(
            "refusing unmanaged image name `{name}`: expected `outbe-vm-(dev|prod)-<16-lowercase-hex>`"
        );
    };
    ensure!(
        matches!(flavor, "dev" | "prod") && is_lower_hex(hash16, 16),
        "refusing unmanaged image name `{name}`: expected `outbe-vm-(dev|prod)-<16-lowercase-hex>`"
    );
    Ok((flavor, hash16))
}

fn verify_managed_image_provenance(image_dir: &Path, name: &str) -> Result<ManagedImageInfo> {
    let (name_flavor, name_hash16) = parse_managed_image_name(name)?;
    let dir_meta = fs::symlink_metadata(image_dir)
        .wrap_err_with(|| format!("stat image directory {}", image_dir.display()))?;
    ensure!(
        dir_meta.is_dir() && !dir_meta.file_type().is_symlink(),
        "image path is not a real directory: {}",
        image_dir.display()
    );

    let manifest_path = image_dir.join(ARCHIVE_MANIFEST_NAME);
    let manifest_meta = fs::symlink_metadata(&manifest_path).wrap_err_with(|| {
        format!(
            "missing required provenance file {}",
            manifest_path.display()
        )
    })?;
    ensure!(
        manifest_meta.is_file() && !manifest_meta.file_type().is_symlink(),
        "provenance manifest is not a regular file: {}",
        manifest_path.display()
    );
    ensure!(
        manifest_meta.len() <= MAX_ARCHIVE_MANIFEST_BYTES,
        "provenance manifest exceeds maximum size: {}",
        manifest_path.display()
    );

    let manifest_raw =
        fs::read(&manifest_path).wrap_err_with(|| format!("read {}", manifest_path.display()))?;
    let manifest: ManagedArchiveManifest = serde_json::from_slice(&manifest_raw)
        .wrap_err("invalid or non-v1 image-archive-manifest.json")?;
    ensure!(
        manifest.version == ARCHIVE_MANIFEST_VERSION,
        "unsupported image-archive-manifest.json version {}",
        manifest.version
    );
    ensure!(
        manifest.flavor == name_flavor,
        "manifest flavor `{}` does not match directory flavor `{name_flavor}`",
        manifest.flavor
    );
    ensure!(
        is_lower_hex(&manifest.os_image_hash, 64),
        "invalid os_image_hash in image-archive-manifest.json"
    );
    ensure!(
        &manifest.os_image_hash[..16] == name_hash16,
        "manifest os_image_hash prefix `{}` does not match directory suffix `{name_hash16}`",
        &manifest.os_image_hash[..16]
    );
    ensure!(
        !manifest.source_commit.is_empty()
            && manifest.source_commit.is_ascii()
            && !manifest.dstack_rev.is_empty()
            && manifest.dstack_rev.is_ascii()
            && !manifest.overlay_commit.is_empty()
            && manifest.overlay_commit.is_ascii(),
        "provenance commit fields in image-archive-manifest.json must be non-empty ASCII"
    );
    ensure!(
        !manifest.files.is_empty()
            && manifest.files.iter().any(|f| f.path == "digest.txt")
            && manifest.files.iter().any(|f| f.path == "metadata.json"),
        "image-archive-manifest.json must declare digest.txt and metadata.json"
    );
    for entry in &manifest.files {
        ensure!(
            !entry.path.is_empty()
                && entry.size <= MAX_DECLARED_FILE_BYTES
                && entry.mode <= 0o7777
                && is_lower_hex(&entry.sha256, 64),
            "invalid file entry `{}` in image-archive-manifest.json",
            entry.path
        );
    }

    let digest_path = image_dir.join("digest.txt");
    let digest_meta = fs::symlink_metadata(&digest_path)
        .wrap_err_with(|| format!("missing digest.txt: {}", digest_path.display()))?;
    ensure!(
        digest_meta.is_file() && !digest_meta.file_type().is_symlink(),
        "digest.txt is not a regular file: {}",
        digest_path.display()
    );
    let digest_actual = fs::read_to_string(&digest_path)
        .wrap_err_with(|| format!("read {}", digest_path.display()))?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        digest_actual == manifest.os_image_hash,
        "digest.txt ({digest_actual}) does not match image-archive-manifest.json os_image_hash ({})",
        manifest.os_image_hash
    );

    Ok(ManagedImageInfo {
        name: name.to_owned(),
        flavor: manifest.flavor,
        os_image_hash: manifest.os_image_hash,
        path: image_dir.to_path_buf(),
        has_archive_manifest: true,
    })
}

fn list_all_vmm_vms<T: VmmTransport>(client: &VmmClient<T>) -> Result<Vec<crate::vmm::VmInfo>> {
    let mut all = Vec::new();
    for page in 1..=MAX_STATUS_PAGES {
        let resp = client.status(&StatusRequest {
            ids: Vec::new(),
            brief: false,
            keyword: String::new(),
            page,
            page_size: STATUS_PAGE_SIZE,
            status: String::new(),
        })?;
        let count = resp.vms.len();
        all.extend(resp.vms);
        if count < STATUS_PAGE_SIZE as usize || (resp.total > 0 && all.len() >= resp.total as usize)
        {
            return Ok(all);
        }
    }
    bail!(
        "VMM Status pagination exceeded {} pages while checking image references",
        MAX_STATUS_PAGES
    );
}

fn copy_image_tree_cancellable(
    src_root: &Path,
    dst_root: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    let mut buf = [0_u8; 64 * 1024];
    for entry in WalkDir::new(src_root).follow_links(false) {
        check_image_canceled(cancel, "staging image tree")?;
        let entry = entry?;
        let path = entry.path();
        if path == src_root {
            continue;
        }
        let rel = path.strip_prefix(src_root)?;
        let dst = dst_root.join(rel);
        let meta = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("stat guest image entry {}", path.display()))?;
        let ft = meta.file_type();
        ensure!(
            !ft.is_symlink(),
            "refusing symlink in guest image directory: {}",
            path.display()
        );
        if ft.is_dir() {
            fs::create_dir_all(&dst)
                .wrap_err_with(|| format!("create directory {}", dst.display()))?;
            fs::set_permissions(&dst, meta.permissions())?;
        } else if ft.is_file() {
            let mut reader = fs::File::open(path)
                .wrap_err_with(|| format!("open {} for staging copy", path.display()))?;
            let mut writer = fs::File::create(&dst)
                .wrap_err_with(|| format!("create {} for staging copy", dst.display()))?;
            loop {
                check_image_canceled(cancel, "copying staged image file")?;
                let n = reader
                    .read(&mut buf)
                    .wrap_err_with(|| format!("read {}", path.display()))?;
                if n == 0 {
                    break;
                }
                writer
                    .write_all(&buf[..n])
                    .wrap_err_with(|| format!("write {}", dst.display()))?;
            }
            writer.flush()?;
            fs::set_permissions(&dst, meta.permissions())?;
        } else {
            bail!(
                "refusing non-regular entry in guest image directory: {}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Build the host shell command that reads `<vmm_home>/image/<image_name>/digest.txt` with sentinels.
pub fn build_remote_image_check_command(vmm_home: &str, image_name: &str) -> String {
    let remote_dir_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}"));
    let remote_digest_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}/digest.txt"));
    format!(
        "if [ ! -e {remote_dir_expr} ]; then printf '{MISSING_SENTINEL}\\n'; else printf '{PRESENT_SENTINEL}\\n' && cat {remote_digest_expr}; fi"
    )
}

/// Build the locked host shell command that atomically publishes a staged guest image directory.
pub fn build_image_publish_command(
    vmm_home: &str,
    image_name: &str,
    stage_dir: &str,
    expected_digest: &str,
) -> String {
    let remote_dir_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}"));
    let remote_digest_expr = remote_vmm_path(vmm_home, &format!("image/{image_name}/digest.txt"));
    let lock_expr = remote_vmm_path(vmm_home, &format!("image/.{image_name}.lock"));
    let stage_quoted = shell_quote(stage_dir);
    let expected_quoted = shell_quote(expected_digest);

    format!(
        "uid=$(id -u); \
         if [ -e {lock_expr} ] || [ -L {lock_expr} ]; then \
           if [ ! -f {lock_expr} ] || [ -L {lock_expr} ] || [ \"$(stat -c '%u' {lock_expr})\" != \"$uid\" ]; then \
             echo 'image lock must be a regular file owned by current uid' >&2; exit 1; \
           fi; \
         fi; \
         exec 9>>{lock_expr}; flock -x 9; \
         if [ -e {remote_dir_expr} ]; then \
           rm -rf {stage_quoted}; \
           if [ ! -f {remote_digest_expr} ]; then echo 'existing image is missing digest.txt' >&2; exit 1; fi; \
           actual=$(tr -d ' \\t\\r\\n' < {remote_digest_expr} | tr 'A-F' 'a-f'); \
           if [ \"$actual\" != {expected_quoted} ]; then echo \"existing image digest mismatch: $actual\" >&2; exit 1; fi; \
         else \
           mv -T {stage_quoted} {remote_dir_expr}; \
         fi; \
         if [ ! -f {remote_digest_expr} ]; then echo 'published image is missing digest.txt' >&2; exit 1; fi; \
         final=$(tr -d ' \\t\\r\\n' < {remote_digest_expr} | tr 'A-F' 'a-f'); \
         if [ \"$final\" != {expected_quoted} ]; then echo \"published image digest mismatch: $final\" >&2; exit 1; fi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_publish_command_handles_normal_and_competitor_race() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let vmm_home = dir.path().to_str().unwrap();
        let image_root = dir.path().join("image");
        fs::create_dir_all(&image_root).unwrap();
        let image_name = "outbe-vm-dev-0123456789abcdef";
        let digest = "0123456789abcdef".repeat(4);

        // 1) Normal publication via `mv -T` + post-publish digest verification.
        let stage1 = image_root.join(".stage1");
        fs::create_dir_all(&stage1).unwrap();
        fs::write(stage1.join("digest.txt"), format!("{digest}\n")).unwrap();
        fs::write(stage1.join("metadata.json"), "{}").unwrap();

        let cmd1 =
            build_image_publish_command(vmm_home, image_name, stage1.to_str().unwrap(), &digest);
        let out1 = Command::new("sh").arg("-c").arg(&cmd1).output().unwrap();
        assert!(
            out1.status.success(),
            "normal publish failed: {}",
            String::from_utf8_lossy(&out1.stderr)
        );
        let published = image_root.join(image_name);
        assert!(published.join("digest.txt").is_file());
        assert!(!stage1.exists());

        // 2) Competitor published first with SAME digest -> cleans stage dir, no nesting, succeeds.
        let stage2 = image_root.join(".stage2");
        fs::create_dir_all(&stage2).unwrap();
        fs::write(stage2.join("digest.txt"), format!("{digest}\n")).unwrap();
        let cmd2 =
            build_image_publish_command(vmm_home, image_name, stage2.to_str().unwrap(), &digest);
        let out2 = Command::new("sh").arg("-c").arg(&cmd2).output().unwrap();
        assert!(
            out2.status.success(),
            "same-digest competitor race should reuse cleanly: {}",
            String::from_utf8_lossy(&out2.stderr)
        );
        assert!(!stage2.exists());
        assert!(
            !published.join(".stage2").exists(),
            "must never nest into target dir"
        );

        // 3) Competitor published first with DIFFERENT digest -> cleans stage dir, refuses, keeps target.
        let other_digest = "f".repeat(64);
        let stage3 = image_root.join(".stage3");
        fs::create_dir_all(&stage3).unwrap();
        fs::write(stage3.join("digest.txt"), format!("{other_digest}\n")).unwrap();
        let cmd3 = build_image_publish_command(
            vmm_home,
            image_name,
            stage3.to_str().unwrap(),
            &other_digest,
        );
        let out3 = Command::new("sh").arg("-c").arg(&cmd3).output().unwrap();
        assert!(
            !out3.status.success(),
            "different-digest competitor must fail"
        );
        assert!(!stage3.exists());
        assert_eq!(
            fs::read_to_string(published.join("digest.txt"))
                .unwrap()
                .trim(),
            digest
        );
    }

    #[test]
    fn digest_qualified_image_name_reuses_match_and_refuses_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("dstack-dev-0.6.3");
        fs::create_dir_all(&image).unwrap();
        assert!(validate_image_dir(&image, None, "dev").is_err());

        fs::write(image.join("metadata.json"), "{}").unwrap();
        let digest = "0123456789abcdef".repeat(4);
        fs::write(image.join("digest.txt"), format!("{digest}\n")).unwrap();

        let (name, parsed_digest) = validate_image_dir(&image, None, "dev").unwrap();
        assert_eq!(name, "outbe-vm-dev-0123456789abcdef");
        assert_eq!(parsed_digest, digest);

        assert_eq!(
            decide_remote_image_action(None, &digest, &name).unwrap(),
            RemoteImageAction::Upload
        );
        assert_eq!(
            decide_remote_image_action(Some(&digest), &digest, &name).unwrap(),
            RemoteImageAction::Reuse
        );
        let err = decide_remote_image_action(Some(&"f".repeat(64)), &digest, &name).unwrap_err();
        assert!(err.to_string().contains("refusing to replace"), "{err}");
    }

    #[test]
    fn publish_local_image_publishes_reuses_and_refuses_digest_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let vmm_home = dir.path().join("vmm");
        let src_a = dir.path().join("src-a");
        fs::create_dir_all(&src_a).unwrap();
        let digest_a = "0123456789abcdef".repeat(4);
        fs::write(src_a.join("metadata.json"), "{\"v\":1}").unwrap();
        fs::write(src_a.join("digest.txt"), format!("{digest_a}\n")).unwrap();
        fs::write(src_a.join("kernel"), b"kernel-bytes").unwrap();

        let first = publish_local_image(&vmm_home, &src_a, None, "dev").unwrap();
        assert!(!first.reused);
        assert_eq!(first.name, "outbe-vm-dev-0123456789abcdef");
        assert!(first.target_dir.join("kernel").is_file());

        let second = publish_local_image(&vmm_home, &src_a, None, "dev").unwrap();
        assert!(second.reused);
        assert_eq!(second.target_dir, first.target_dir);

        // Different digest under the same explicit image name must be refused.
        let src_b = dir.path().join("src-b");
        fs::create_dir_all(&src_b).unwrap();
        let digest_b = "f".repeat(64);
        fs::write(src_b.join("metadata.json"), "{\"v\":2}").unwrap();
        fs::write(src_b.join("digest.txt"), format!("{digest_b}\n")).unwrap();
        let err = publish_local_image(&vmm_home, &src_b, Some(&first.name), "dev").unwrap_err();
        assert!(err.to_string().contains("refusing to replace"), "{err}");
    }

    struct MockVmmTransport {
        status_json: Result<String, String>,
    }

    impl VmmTransport for MockVmmTransport {
        fn prpc_post(&self, method: &str, _payload: &[u8]) -> Result<String> {
            ensure!(method == "Status", "unexpected method {method}");
            match &self.status_json {
                Ok(body) => Ok(body.clone()),
                Err(msg) => Err(eyre::eyre!("{msg}")),
            }
        }

        fn read_serial_log(&self, _vm_id: &str, _lines: usize) -> Result<String> {
            Ok(String::new())
        }
    }

    fn write_valid_archive_manifest(dir: &Path, flavor: &str, digest: &str) {
        let manifest = serde_json::json!({
            "version": 1,
            "flavor": flavor,
            "os_image_hash": digest,
            "source_commit": "a".repeat(40),
            "dstack_rev": "b".repeat(40),
            "overlay_commit": "c".repeat(40),
            "files": [
                {
                    "path": "digest.txt",
                    "size": digest.len() + 1,
                    "sha256": "0".repeat(64),
                    "mode": 0o644
                },
                {
                    "path": "metadata.json",
                    "size": 2,
                    "sha256": "1".repeat(64),
                    "mode": 0o644
                }
            ]
        });
        fs::write(
            dir.join(ARCHIVE_MANIFEST_NAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn list_and_remove_local_images_enforces_provenance_vmm_references_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let vmm_home = dir.path().join("vmm");
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let digest = "0123456789abcdef".repeat(4);
        fs::write(src.join("metadata.json"), "{}").unwrap();
        fs::write(src.join("digest.txt"), format!("{digest}\n")).unwrap();
        write_valid_archive_manifest(&src, "dev", &digest);
        let published = publish_local_image(&vmm_home, &src, None, "dev").unwrap();

        // Plant foreign / unproven directories; list must omit all of them and remove must refuse.
        let foreign = vmm_home.join("image/important-vendor-image");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("digest.txt"), format!("{digest}\n")).unwrap();
        fs::write(foreign.join("metadata.json"), "{}").unwrap();

        let no_manifest = vmm_home.join("image/outbe-vm-dev-1111111111111111");
        fs::create_dir_all(&no_manifest).unwrap();
        fs::write(
            no_manifest.join("digest.txt"),
            format!("{}\n", "1".repeat(64)),
        )
        .unwrap();
        fs::write(no_manifest.join("metadata.json"), "{}").unwrap();

        let empty_manifest = vmm_home.join("image/outbe-vm-dev-2222222222222222");
        fs::create_dir_all(&empty_manifest).unwrap();
        fs::write(
            empty_manifest.join("digest.txt"),
            format!("{}\n", "2".repeat(64)),
        )
        .unwrap();
        fs::write(empty_manifest.join("metadata.json"), "{}").unwrap();
        fs::write(empty_manifest.join(ARCHIVE_MANIFEST_NAME), "{}").unwrap();

        let mismatched_manifest = vmm_home.join("image/outbe-vm-dev-3333333333333333");
        fs::create_dir_all(&mismatched_manifest).unwrap();
        fs::write(
            mismatched_manifest.join("digest.txt"),
            format!("{}\n", "3".repeat(64)),
        )
        .unwrap();
        fs::write(mismatched_manifest.join("metadata.json"), "{}").unwrap();
        write_valid_archive_manifest(&mismatched_manifest, "prod", &"3".repeat(64));

        let listed = list_local_images(&vmm_home).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, published.name);
        assert_eq!(listed[0].flavor, "dev");
        assert_eq!(listed[0].os_image_hash, digest);

        let empty_client = VmmClient::new(
            MockVmmTransport {
                status_json: Ok(r#"{"vms":[],"total":0}"#.to_owned()),
            },
            "mock-empty",
        );
        for (bad_name, bad_dir) in [
            ("important-vendor-image", &foreign),
            ("outbe-vm-dev-1111111111111111", &no_manifest),
            ("outbe-vm-dev-2222222222222222", &empty_manifest),
            ("outbe-vm-dev-3333333333333333", &mismatched_manifest),
        ] {
            let err = remove_local_image(&vmm_home, &empty_client, bad_name).unwrap_err();
            assert!(
                format!("{err:#}").contains("refusing"),
                "expected refusal for {bad_name}: {err:#}"
            );
            assert!(
                bad_dir.is_dir(),
                "unmanaged dir {bad_name} must remain intact"
            );
        }

        // 1. Unreachable VMM fails closed and preserves the image.
        let down_client = VmmClient::new(
            MockVmmTransport {
                status_json: Err("connection refused".to_owned()),
            },
            "mock-down",
        );
        let err = remove_local_image(&vmm_home, &down_client, &published.name).unwrap_err();
        assert!(
            format!("{err:#}").contains("failed to query VMM"),
            "{err:#}"
        );
        assert!(published.target_dir.is_dir());

        // 2. Referenced by a stopped VM -> refused and preserved.
        let busy_json = serde_json::json!({
            "vms": [{
                "id": "11111111-2222-3333-4444-555555555555",
                "name": "node-a",
                "status": "stopped",
                "running": false,
                "configuration": { "image": published.name }
            }],
            "total": 1
        })
        .to_string();
        let busy_client = VmmClient::new(
            MockVmmTransport {
                status_json: Ok(busy_json),
            },
            "mock-busy",
        );
        let err = remove_local_image(&vmm_home, &busy_client, &published.name).unwrap_err();
        assert!(
            err.to_string()
                .contains("still referenced by VM(s): node-a"),
            "{err}"
        );
        assert!(published.target_dir.is_dir());

        // 3. Zero referencing VMs -> removed cleanly.
        let removed_path = remove_local_image(&vmm_home, &empty_client, &published.name).unwrap();
        assert_eq!(removed_path, published.target_dir);
        assert!(!published.target_dir.exists());
        assert!(list_local_images(&vmm_home).unwrap().is_empty());
    }

    #[test]
    fn publish_verified_cancellable_refuses_corrupt_reuse_and_aborts_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let vmm_home = dir.path().join("vmm");
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let digest = "abcdef0123456789".repeat(4);
        fs::write(src.join("metadata.json"), "{}").unwrap();
        fs::write(src.join("digest.txt"), format!("{digest}\n")).unwrap();
        write_valid_archive_manifest(&src, "dev", &digest);

        // Pre-canceled flag must abort without publishing.
        let canceled = AtomicBool::new(true);
        let err = publish_local_image_verified_cancellable(
            &vmm_home,
            &src,
            None,
            "dev",
            &canceled,
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("canceled"), "{err}");
        assert!(!vmm_home
            .join("image/outbe-vm-dev-abcdef0123456789")
            .exists());

        // Publish normally, then verify_existing failure refuses reuse.
        let active = AtomicBool::new(false);
        let published =
            publish_local_image_verified_cancellable(&vmm_home, &src, None, "dev", &active, |_| {
                Ok(())
            })
            .unwrap();
        assert!(!published.reused);

        let err =
            publish_local_image_verified_cancellable(&vmm_home, &src, None, "dev", &active, |_| {
                Err(eyre::eyre!("corrupt rootfs.squashfs"))
            })
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("corrupt rootfs.squashfs"),
            "{err:#}"
        );
    }
}
