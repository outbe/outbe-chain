//! Per-role VM data-disk tree staging and static validation (`outbe-up disk prepare`).
//!
//! ## Signal cancellation and stale staging cleanup
//!
//! All long-running staging steps (`bundle::verify_and_extract_cancellable`, founder tar
//! decompression, `outbe-keygen validator`, and key copy) honor a cooperative `AtomicBool`
//! cancellation flag so `SIGINT`/`SIGTERM` promptly terminates child process groups
//! (`SIGTERM` followed by `SIGKILL` after 500 ms) and drops temporary staging directories
//! (`.outbe-prepare-*`, `.outbe-disk-*`).
//!
//! An uncatchable `SIGKILL` (or sudden host power loss) cannot run in-process `Drop` handlers;
//! [`cleanup_stale_staging_dirs`] is provided behind an explicit `--cleanup-stale` CLI flag to
//! remove leftover staging directories owned by the current UID.

use std::{
    cell::RefCell,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use alloy_primitives::Address;
use eyre::{bail, ensure, Context, Result};
use rustix::{
    fd::OwnedFd,
    fs::{
        fcntl_getfl, fcntl_setfl, flock, fstat, openat, FileType, FlockOperation, Mode, OFlags, CWD,
    },
    io::Errno,
    process::{getuid, kill_process_group, test_kill_process_group, Pid, Signal},
};
use walkdir::WalkDir;

use crate::{
    bundle,
    config::{FullNodeConfig, NodeRole, OperatorConfig},
    path_text, validator_address, vm_boot,
};

const MAX_FOUNDER_TAR_ENTRIES: usize = 2048;
/// Maximum total decompressed byte budget across the entire founder tar stream (headers,
/// padding, GNU/PAX extended headers, and payloads).
const MAX_FOUNDER_TAR_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum decompressed metadata bytes (headers + GNU longname/longlink + PAX extended headers)
/// allowed before a single tar entry is yielded, preventing pre-yield OOM allocations.
const MAX_TAR_HEADER_METADATA_BYTES: u64 = 1024 * 1024;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

const STALE_STAGING_PREFIXES: &[&str] = &[".outbe-prepare-", ".outbe-disk-", ".outbe-vm-disk-"];

const ALLOWED_KEYS_DIRS: &[&str] = &["radicle", "radicle/keys"];
const ALLOWED_KEYS_FILES: &[&str] = &[
    "signing-key.hex",
    "evm-key.hex",
    "feeder-key.hex",
    "hyperlane-key.hex",
    "reth-p2p-secret.hex",
    "radicle/keys/radicle",
    "radicle/keys/radicle.pub",
];

/// How `prepare_founder_tree_cancellable` locates `outbe-keygen` to verify `signing-key.hex`.
#[derive(Clone, Copy, Debug)]
pub enum FounderKeyVerify<'a> {
    /// Auto-discover `<artifacts>/joining/outbe-validator-operator-bundle.tar.zst` next to a
    /// founder source inside network artifacts; fail closed if no verified bundle is found.
    Auto,
    /// Verify and extract the specified operator bundle (`.tar.zst`) and run its `outbe-keygen`.
    Bundle(&'a Path),
    /// Run `outbe-keygen` directly from a pre-verified binary directory (primarily for tests).
    BinDir(&'a Path),
    /// Explicitly skip `outbe-keygen verify` (`--skip-key-verify`).
    Skip,
}

struct LockedTempDir {
    // Field order matters: `dir` drops (`remove_dir_all`) BEFORE `_lock` releases the exclusive
    // directory `flock`, preventing `--cleanup-stale` from racing with teardown.
    dir: tempfile::TempDir,
    _lock: OwnedFd,
}

impl LockedTempDir {
    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Verified static VM data tree ready to be packed into a deterministic ext4 disk image.
pub struct StagedDiskTree {
    tree_path: PathBuf,
    _staging: Option<LockedTempDir>,
    _bundle: Option<bundle::ExtractedBundle>,
    pub role: NodeRole,
    pub validator_address: Option<Address>,
    pub hyperlane_address: Option<Address>,
    /// Whether `outbe-keygen verify` ran and succeeded on `keys/signing-key.hex`.
    pub key_verified: bool,
    /// Human-readable source of `outbe-keygen` used for key verification, if any.
    pub keygen_source: Option<String>,
}

impl StagedDiskTree {
    pub fn tree_path(&self) -> &Path {
        &self.tree_path
    }
}

/// Remove stale `.outbe-prepare-*`, `.outbe-disk-*`, and `.outbe-vm-disk-*` staging directories
/// inside `staging_parent` that are real directories (not symlinks) owned by the current UID and
/// NOT currently locked by a live `outbe-up` process.
///
/// Acquires an exclusive `flock` on `staging_parent` during the scan (serialized against the brief
/// shared `flock` held while live processes create and lock new staging dirs) and probes each
/// candidate directory with `NonBlockingLockExclusive` so live staging directories are never
/// deleted.
///
/// Only intended to be called when the operator explicitly passes `--cleanup-stale` (for example
/// after an uncatchable `SIGKILL` or host power loss).
pub fn cleanup_stale_staging_dirs(staging_parent: &Path) -> Result<Vec<PathBuf>> {
    let parent_fd = match openat(
        CWD,
        staging_parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(Vec::new()),
        Err(err) => {
            return Err(io::Error::from(err)).wrap_err_with(|| {
                format!(
                    "failed to open staging directory parent {}",
                    staging_parent.display()
                )
            });
        }
    };
    flock(&parent_fd, FlockOperation::LockExclusive)
        .map_err(io::Error::from)
        .wrap_err_with(|| {
            format!(
                "failed to lock staging directory parent {}",
                staging_parent.display()
            )
        })?;

    let entries = fs::read_dir(staging_parent).wrap_err_with(|| {
        format!(
            "failed to read staging directory parent {}",
            staging_parent.display()
        )
    })?;

    let my_uid = getuid().as_raw();
    let mut removed = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !STALE_STAGING_PREFIXES
            .iter()
            .any(|prefix| name_str.starts_with(prefix))
        {
            continue;
        }
        let path = entry.path();
        let cand_fd = match openat(
            CWD,
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => continue,
            Err(err) => {
                return Err(io::Error::from(err)).wrap_err_with(|| {
                    format!("failed to open staging candidate {}", path.display())
                });
            }
        };
        let st = fstat(&cand_fd)
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("failed to stat {}", path.display()))?;
        if FileType::from_raw_mode(st.st_mode) != FileType::Directory || st.st_uid != my_uid {
            continue;
        }
        match flock(&cand_fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                // Held exclusively by a live `outbe-up` process; never touch it.
                continue;
            }
            Err(err) => {
                return Err(io::Error::from(err))
                    .wrap_err_with(|| format!("failed to probe lock on {}", path.display()));
            }
        }
        fs::remove_dir_all(&path)
            .wrap_err_with(|| format!("failed to remove stale staging dir {}", path.display()))?;
        drop(cand_fd);
        removed.push(path);
    }
    drop(parent_fd);
    Ok(removed)
}

/// Validate a founder VM data tree (directory or tar archive) without mutating a directory input.
///
/// Non-cancellable convenience wrapper around [`prepare_founder_tree_cancellable`].
pub fn prepare_founder_tree(
    source: &Path,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
) -> Result<StagedDiskTree> {
    let cancel = AtomicBool::new(false);
    prepare_founder_tree_cancellable(source, staging_parent, verify, &cancel)
}

/// Validate a founder VM data tree (directory or tar archive) with cooperative signal
/// cancellation and verified `outbe-keygen` BLS key verification (unless `FounderKeyVerify::Skip`).
///
/// When `source` is a directory, it is verified in place and returned directly so the resulting
/// raw disk image is byte-identical to `outbe-up vm disk` for the same input tree.
pub fn prepare_founder_tree_cancellable(
    source: &Path,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    ensure_not_canceled(cancel, "founder staging")?;
    let meta = fs::symlink_metadata(source)
        .wrap_err_with(|| format!("founder source does not exist: {}", source.display()))?;
    ensure!(
        !meta.file_type().is_symlink(),
        "refusing symlink founder source: {}",
        source.display()
    );

    let (bin_dir, bundle, key_verified, keygen_source) =
        resolve_founder_keygen(Some(source), staging_parent, verify, cancel)?;
    ensure_not_canceled(cancel, "founder staging")?;

    if meta.is_dir() {
        vm_boot::verify_static_tree_for_role_cancellable(
            source,
            NodeRole::Founder,
            bin_dir.as_deref(),
            cancel,
        )?;
        let validator = validator_address(&source.join("keys/evm-key.hex"))?;
        let hyperlane = validator_address(&source.join("keys/hyperlane-key.hex"))?;
        return Ok(StagedDiskTree {
            tree_path: source.to_path_buf(),
            _staging: None,
            _bundle: bundle,
            role: NodeRole::Founder,
            validator_address: Some(validator),
            hyperlane_address: Some(hyperlane),
            key_verified,
            keygen_source,
        });
    }

    ensure!(
        meta.is_file(),
        "founder source must be a directory or tar archive: {}",
        source.display()
    );

    let staging = create_locked_staging_dir(staging_parent, ".outbe-disk-founder-")?;
    let extract_root = staging.path().join("extract");
    create_dir_mode(&extract_root, 0o755)?;
    extract_founder_tar(source, &extract_root, cancel)?;
    ensure_not_canceled(cancel, "founder staging")?;
    let tree_path = resolve_extracted_founder_root(&extract_root)?;
    fs::set_permissions(&tree_path, fs::Permissions::from_mode(0o755))?;

    vm_boot::verify_static_tree_for_role_cancellable(
        &tree_path,
        NodeRole::Founder,
        bin_dir.as_deref(),
        cancel,
    )?;
    let validator = validator_address(&tree_path.join("keys/evm-key.hex"))?;
    let hyperlane = validator_address(&tree_path.join("keys/hyperlane-key.hex"))?;

    Ok(StagedDiskTree {
        tree_path,
        _staging: Some(staging),
        _bundle: bundle,
        role: NodeRole::Founder,
        validator_address: Some(validator),
        hyperlane_address: Some(hyperlane),
        key_verified,
        keygen_source,
    })
}

/// Validate a founder VM data tree streamed as a tar archive (plain `.tar`, `.tar.zst`, or
/// `.tar.gz`) from `reader`, extracting only into a private `0700` locked staging directory under
/// `staging_parent`.
///
/// Non-cancellable convenience wrapper around [`prepare_founder_tree_from_reader_cancellable`].
pub fn prepare_founder_tree_from_reader<R: Read + Send>(
    reader: R,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
) -> Result<StagedDiskTree> {
    let cancel = AtomicBool::new(false);
    prepare_founder_tree_from_reader_cancellable(reader, staging_parent, verify, &cancel)
}

/// Validate a founder VM data tree streamed as a tar archive (plain `.tar`, `.tar.zst`, or
/// `.tar.gz`) from `reader` with cooperative signal cancellation and verified `outbe-keygen`
/// BLS key verification (unless `FounderKeyVerify::Skip`).
///
/// `FounderKeyVerify::Auto` is refused because stdin has no filesystem path from which to
/// auto-discover `<artifacts>/joining/outbe-validator-operator-bundle.tar.zst`; callers must pass
/// `FounderKeyVerify::Bundle(...)`, `FounderKeyVerify::BinDir(...)`, or `FounderKeyVerify::Skip`.
pub fn prepare_founder_tree_from_reader_cancellable<R: Read + Send>(
    reader: R,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    prepare_validator_tree_from_reader_for_role(
        reader,
        NodeRole::Founder,
        staging_parent,
        verify,
        cancel,
    )
}

/// Validate a joining-validator VM data tree streamed as a tar archive (plain `.tar`, `.tar.zst`,
/// or `.tar.gz`) from `reader`, extracting only into a private `0700` locked staging directory.
pub fn prepare_joining_validator_tree_from_reader<R: Read + Send>(
    reader: R,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
) -> Result<StagedDiskTree> {
    let cancel = AtomicBool::new(false);
    prepare_joining_validator_tree_from_reader_cancellable(reader, staging_parent, verify, &cancel)
}

/// Validate a joining-validator VM data tree streamed as a tar archive (plain `.tar`, `.tar.zst`,
/// or `.tar.gz`) from `reader` with cooperative signal cancellation and verified `outbe-keygen`
/// BLS key verification.
pub fn prepare_joining_validator_tree_from_reader_cancellable<R: Read + Send>(
    reader: R,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    prepare_validator_tree_from_reader_for_role(
        reader,
        NodeRole::JoiningValidator,
        staging_parent,
        verify,
        cancel,
    )
}

fn prepare_validator_tree_from_reader_for_role<R: Read + Send>(
    reader: R,
    expected_role: NodeRole,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    ensure!(
        matches!(expected_role, NodeRole::Founder | NodeRole::JoiningValidator),
        "stdin validator tar staging only supports founder or joining-validator (got {expected_role})"
    );
    let stage_label = format!("{expected_role} staging");
    ensure_not_canceled(cancel, &stage_label)?;

    let (bin_dir, bundle, key_verified, keygen_source) =
        resolve_founder_keygen(None, staging_parent, verify, cancel)?;
    ensure_not_canceled(cancel, &stage_label)?;

    let prefix = match expected_role {
        NodeRole::Founder => ".outbe-disk-founder-",
        NodeRole::JoiningValidator => ".outbe-disk-joining-",
        NodeRole::FullNode => unreachable!("guarded above"),
    };
    let staging = create_locked_staging_dir(staging_parent, prefix)?;
    let extract_root = staging.path().join("extract");
    create_dir_mode(&extract_root, 0o755)?;
    extract_founder_tar_reader(reader, &extract_root, cancel)?;
    ensure_not_canceled(cancel, &stage_label)?;
    let tree_path = resolve_extracted_founder_root(&extract_root)?;
    fs::set_permissions(&tree_path, fs::Permissions::from_mode(0o755))?;

    vm_boot::verify_static_tree_for_role_cancellable(
        &tree_path,
        expected_role,
        bin_dir.as_deref(),
        cancel,
    )?;
    let validator = validator_address(&tree_path.join("keys/evm-key.hex"))?;
    let hyperlane = validator_address(&tree_path.join("keys/hyperlane-key.hex"))?;

    Ok(StagedDiskTree {
        tree_path,
        _staging: Some(staging),
        _bundle: bundle,
        role: expected_role,
        validator_address: Some(validator),
        hyperlane_address: Some(hyperlane),
        key_verified,
        keygen_source,
    })
}

/// Assemble and verify a joining-validator VM data tree (non-cancellable convenience wrapper).
pub fn prepare_joining_validator_tree(
    bundle_path: &Path,
    config_path: &Path,
    keys_from: Option<&Path>,
    staging_parent: &Path,
) -> Result<StagedDiskTree> {
    let cancel = AtomicBool::new(false);
    prepare_joining_validator_tree_cancellable(
        bundle_path,
        config_path,
        keys_from,
        staging_parent,
        &cancel,
    )
}

/// Assemble and verify a joining-validator VM data tree from an operator bundle and operator
/// config, generating keys locally via the bundle's verified `outbe-keygen` (or copying
/// deterministic test keys from `keys_from`), with cooperative signal cancellation.
pub fn prepare_joining_validator_tree_cancellable(
    bundle_path: &Path,
    config_path: &Path,
    keys_from: Option<&Path>,
    staging_parent: &Path,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    ensure_not_canceled(cancel, "joining-validator staging")?;
    let extracted = bundle::verify_and_extract_cancellable(bundle_path, staging_parent, cancel)?;
    let bundle_root = extracted.directory.path();

    let (config, _) = OperatorConfig::read(config_path)?;
    verify_bundle_chain_id(bundle_root, config.network.chain_id)?;
    ensure_not_canceled(cancel, "joining-validator staging")?;

    let staging = create_locked_staging_dir(staging_parent, ".outbe-disk-joining-")?;
    let tree = staging.path().join("tree");
    create_dir_mode(&tree, 0o755)?;

    copy_static_bundle_assets(bundle_root, &tree)?;

    let keys_dir = tree.join("keys");
    create_dir_mode(&keys_dir, 0o700)?;
    match keys_from {
        Some(source_keys) => copy_keys_strict(source_keys, &keys_dir, cancel)?,
        None => generate_keys_with_bundle_keygen(
            bundle_root,
            &keys_dir,
            config.network.chain_id,
            cancel,
        )?,
    }
    ensure_not_canceled(cancel, "joining-validator staging")?;
    normalize_keys_tree(&keys_dir)?;

    let validator = validator_address(&keys_dir.join("evm-key.hex"))?;
    let hyperlane = validator_address(&keys_dir.join("hyperlane-key.hex"))?;

    config.write_runtime_config_for_role(&tree, validator, NodeRole::JoiningValidator)?;
    for name in [
        "validator.env",
        "feeder-public.toml",
        "hyperlane-public.toml",
    ] {
        fs::set_permissions(tree.join(name), fs::Permissions::from_mode(0o640))?;
    }

    ensure_not_canceled(cancel, "joining-validator verification")?;
    vm_boot::verify_static_tree_for_role_cancellable(
        &tree,
        NodeRole::JoiningValidator,
        Some(bundle_root),
        cancel,
    )?;

    Ok(StagedDiskTree {
        tree_path: tree,
        _staging: Some(staging),
        _bundle: Some(extracted),
        role: NodeRole::JoiningValidator,
        validator_address: Some(validator),
        hyperlane_address: Some(hyperlane),
        key_verified: true,
        keygen_source: Some(format!("bundle:{}", bundle_path.display())),
    })
}

/// Assemble and verify a keyless full-node VM data tree (non-cancellable convenience wrapper).
pub fn prepare_full_node_tree(
    bundle_path: &Path,
    config_path: &Path,
    keys_from: Option<&Path>,
    staging_parent: &Path,
) -> Result<StagedDiskTree> {
    let cancel = AtomicBool::new(false);
    prepare_full_node_tree_cancellable(bundle_path, config_path, keys_from, staging_parent, &cancel)
}

/// Assemble and verify a keyless full-node VM data tree from an operator bundle and strict
/// [`FullNodeConfig`], with cooperative signal cancellation. Writes zero private keys on disk.
pub fn prepare_full_node_tree_cancellable(
    bundle_path: &Path,
    config_path: &Path,
    keys_from: Option<&Path>,
    staging_parent: &Path,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    ensure_not_canceled(cancel, "full-node staging")?;
    ensure!(
        keys_from.is_none(),
        "full-node role uses zero keys on disk; --keys-from is not allowed for full-node"
    );

    let extracted = bundle::verify_and_extract_cancellable(bundle_path, staging_parent, cancel)?;
    let bundle_root = extracted.directory.path();

    let (config, _) = FullNodeConfig::read(config_path)?;
    verify_bundle_chain_id(bundle_root, config.network.chain_id)?;
    ensure_not_canceled(cancel, "full-node staging")?;

    let staging = create_locked_staging_dir(staging_parent, ".outbe-disk-fullnode-")?;
    let tree = staging.path().join("tree");
    create_dir_mode(&tree, 0o755)?;

    copy_static_bundle_assets(bundle_root, &tree)?;

    config.write_runtime_config(&tree)?;
    fs::set_permissions(
        tree.join("validator.env"),
        fs::Permissions::from_mode(0o640),
    )?;

    ensure_not_canceled(cancel, "full-node verification")?;
    vm_boot::verify_static_tree_for_role_cancellable(&tree, NodeRole::FullNode, None, cancel)?;

    Ok(StagedDiskTree {
        tree_path: tree,
        _staging: Some(staging),
        _bundle: Some(extracted),
        role: NodeRole::FullNode,
        validator_address: None,
        hyperlane_address: None,
        key_verified: false,
        keygen_source: None,
    })
}

#[allow(clippy::type_complexity)]
fn resolve_founder_keygen(
    source: Option<&Path>,
    staging_parent: &Path,
    verify: FounderKeyVerify<'_>,
    cancel: &AtomicBool,
) -> Result<(
    Option<PathBuf>,
    Option<bundle::ExtractedBundle>,
    bool,
    Option<String>,
)> {
    match verify {
        FounderKeyVerify::Skip => Ok((None, None, false, None)),
        FounderKeyVerify::BinDir(dir) => {
            ensure!(
                dir.join("outbe-keygen").is_file(),
                "outbe-keygen binary is missing in {}",
                dir.display()
            );
            Ok((
                Some(dir.to_path_buf()),
                None,
                true,
                Some(format!("bin-dir:{}", dir.display())),
            ))
        }
        FounderKeyVerify::Bundle(bundle_path) => {
            let extracted =
                bundle::verify_and_extract_cancellable(bundle_path, staging_parent, cancel)?;
            let dir_path = extracted.directory.path().to_path_buf();
            ensure!(
                dir_path.join("outbe-keygen").is_file(),
                "operator bundle is missing outbe-keygen binary: {}",
                bundle_path.display()
            );
            Ok((
                Some(dir_path),
                Some(extracted),
                true,
                Some(format!("bundle:{}", bundle_path.display())),
            ))
        }
        FounderKeyVerify::Auto => {
            let Some(source_path) = source else {
                bail!(
                    "cannot auto-discover operator bundle when reading founder tar from stdin (`--source -`); \
                     pass --bundle <operator-bundle.tar.zst> to run outbe-keygen verify or pass \
                     --skip-key-verify to explicitly skip BLS key verification"
                );
            };
            let Some(discovered) = discover_artifacts_operator_bundle(source_path)? else {
                bail!(
                    "cannot verify founder signing-key.hex: no verified operator bundle found at \
                     <artifacts>/joining/outbe-validator-operator-bundle.tar.zst; pass --bundle \
                     <operator-bundle.tar.zst> to run outbe-keygen verify or pass --skip-key-verify \
                     to explicitly skip BLS key verification"
                );
            };
            let extracted =
                bundle::verify_and_extract_cancellable(&discovered, staging_parent, cancel)?;
            let dir_path = extracted.directory.path().to_path_buf();
            ensure!(
                dir_path.join("outbe-keygen").is_file(),
                "discovered operator bundle is missing outbe-keygen binary: {}",
                discovered.display()
            );
            Ok((
                Some(dir_path),
                Some(extracted),
                true,
                Some(format!("artifacts:{}", discovered.display())),
            ))
        }
    }
}

fn discover_artifacts_operator_bundle(source: &Path) -> Result<Option<PathBuf>> {
    let canonical = fs::canonicalize(source)
        .wrap_err_with(|| format!("failed to canonicalize founder source {}", source.display()))?;
    let Some(parent) = canonical.parent() else {
        return Ok(None);
    };
    let parent_name = parent.file_name().and_then(|name| name.to_str());
    if !matches!(parent_name, Some("vm" | "dist")) {
        return Ok(None);
    }
    let Some(artifacts_root) = parent.parent() else {
        return Ok(None);
    };
    let candidate = artifacts_root.join("joining/outbe-validator-operator-bundle.tar.zst");
    match fs::symlink_metadata(&candidate) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(Some(candidate)),
        _ => Ok(None),
    }
}

fn ensure_not_canceled(cancel: &AtomicBool, stage: &str) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "{stage} canceled by signal"
    );
    Ok(())
}

fn create_locked_staging_dir(parent: &Path, prefix: &str) -> Result<LockedTempDir> {
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("failed to create staging parent {}", parent.display()))?;
    let parent_fd = openat(
        CWD,
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("failed to open staging parent {}", parent.display()))?;
    flock(&parent_fd, FlockOperation::LockShared)
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to lock staging parent {}", parent.display()))?;

    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .wrap_err_with(|| format!("failed to create staging tempdir in {}", parent.display()))?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    let lock = openat(
        CWD,
        dir.path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| {
        format!(
            "failed to open staging dir {} for flock",
            dir.path().display()
        )
    })?;
    flock(&lock, FlockOperation::LockExclusive)
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to flock staging dir {}", dir.path().display()))?;
    drop(parent_fd);
    Ok(LockedTempDir { dir, _lock: lock })
}

fn create_dir_mode(path: &Path, mode: u32) -> Result<()> {
    DirBuilder::new()
        .mode(mode)
        .create(path)
        .wrap_err_with(|| format!("failed to create directory {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn write_file_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .wrap_err_with(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn copy_file_mode(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut src = File::open(source)
        .wrap_err_with(|| format!("failed to open source file {}", source.display()))?;
    let mut dst = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(destination)
        .wrap_err_with(|| format!("failed to create {}", destination.display()))?;
    io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn verify_bundle_chain_id(bundle_root: &Path, expected_chain_id: u64) -> Result<()> {
    let genesis = bundle_root.join("genesis.json");
    ensure!(
        genesis.is_file(),
        "operator bundle is missing genesis.json: {}",
        genesis.display()
    );
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&genesis)?)
        .wrap_err("invalid genesis.json in operator bundle")?;
    let actual = value
        .pointer("/config/chainId")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| eyre::eyre!("genesis.json has no numeric config.chainId"))?;
    ensure!(
        actual == expected_chain_id,
        "genesis.json chain id ({actual}) does not match config chain_id ({expected_chain_id})"
    );
    Ok(())
}

fn copy_static_bundle_assets(bundle_root: &Path, tree: &Path) -> Result<()> {
    let genesis_src = bundle_root.join("genesis.json");
    ensure!(
        genesis_src.is_file(),
        "operator bundle is missing genesis.json"
    );
    let crs_src = bundle_root.join(".bb-crs/bn254_g1.dat");
    ensure!(
        crs_src.is_file(),
        "operator bundle is missing .bb-crs/bn254_g1.dat"
    );
    copy_file_mode(&genesis_src, &tree.join("genesis.json"), 0o644)?;
    create_dir_mode(&tree.join(".bb-crs"), 0o755)?;
    copy_file_mode(&crs_src, &tree.join(".bb-crs/bn254_g1.dat"), 0o644)?;
    Ok(())
}

fn generate_keys_with_bundle_keygen(
    bundle_root: &Path,
    keys_dir: &Path,
    chain_id: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_not_canceled(cancel, "outbe-keygen validator")?;
    let keygen = bundle_root.join("outbe-keygen");
    ensure!(
        keygen.is_file(),
        "operator bundle is missing outbe-keygen binary: {}",
        keygen.display()
    );
    // Discard stdout/stderr to /dev/null so keygen output is never buffered or emitted to logs.
    let mut cmd = Command::new(&keygen);
    cmd.args([
        "validator",
        "--output-dir",
        path_text(keys_dir)?,
        "--chain-id",
        &chain_id.to_string(),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0);
    let mut child = cmd
        .spawn()
        .wrap_err_with(|| format!("failed to execute {}", keygen.display()))?;
    let status = wait_child_cancellable(&mut child, cancel, "outbe-keygen validator")?;
    ensure!(
        status.success(),
        "{} validator failed with {status}",
        keygen.display()
    );
    Ok(())
}

fn wait_child_cancellable(
    child: &mut Child,
    cancel: &AtomicBool,
    label: &str,
) -> Result<std::process::ExitStatus> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child.id());
            let _ = child.wait();
            bail!("{label} canceled by signal");
        }
        if let Some(status) = child
            .try_wait()
            .wrap_err_with(|| format!("wait for {label}"))?
        {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn terminate_process_group(pid: u32) {
    let Some(pgid) = Pid::from_raw(pid as i32) else {
        return;
    };
    let _ = kill_process_group(pgid, Signal::TERM);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if test_kill_process_group(pgid).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = kill_process_group(pgid, Signal::KILL);
}

fn normalize_relative_key_path(rel: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let text = part
                    .to_str()
                    .ok_or_else(|| eyre::eyre!("non-UTF-8 path in --keys-from"))?;
                parts.push(text);
            }
            _ => {
                return Err(eyre::eyre!(
                    "invalid path in --keys-from: {}",
                    rel.display()
                ));
            }
        }
    }
    Ok(parts.join("/"))
}

fn copy_keys_strict(source: &Path, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    let meta = fs::symlink_metadata(source)
        .wrap_err_with(|| format!("keys directory does not exist: {}", source.display()))?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "keys source is not a directory: {}",
        source.display()
    );
    for entry in WalkDir::new(source).follow_links(false) {
        ensure_not_canceled(cancel, "copy keys")?;
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(source)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_key = normalize_relative_key_path(rel)?;
        let ft = fs::symlink_metadata(path)?.file_type();
        ensure!(
            !ft.is_symlink(),
            "refusing symlink in keys directory: {}",
            path.display()
        );
        let target = destination.join(rel);
        if ft.is_dir() {
            ensure!(
                ALLOWED_KEYS_DIRS.contains(&rel_key.as_str()),
                "refusing unexpected directory in --keys-from: {rel_key}"
            );
            create_dir_mode(&target, 0o700)?;
        } else if ft.is_file() {
            ensure!(
                ALLOWED_KEYS_FILES.contains(&rel_key.as_str()),
                "refusing unexpected file in --keys-from: {rel_key}"
            );
            let mode = entry.metadata()?.permissions().mode() & 0o7777;
            ensure!(
                mode & 0o111 == 0,
                "refusing executable file in keys directory: {}",
                path.display()
            );
            copy_file_mode(path, &target, 0o600)?;
        } else {
            bail!(
                "refusing non-regular entry in keys directory: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn normalize_keys_tree(keys_dir: &Path) -> Result<()> {
    fs::set_permissions(keys_dir, fs::Permissions::from_mode(0o700))?;
    for entry in WalkDir::new(keys_dir).follow_links(false) {
        let entry = entry?;
        let path = entry.path();
        let ft = fs::symlink_metadata(path)?.file_type();
        ensure!(
            !ft.is_symlink(),
            "refusing symlink in keys directory: {}",
            path.display()
        );
        if ft.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        } else if ft.is_file() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        } else {
            bail!(
                "refusing non-regular entry in keys directory: {}",
                path.display()
            );
        }
    }
    // Strip trailing whitespace from reth-p2p-secret.hex so it matches canonical hex validation.
    let p2p_key = keys_dir.join("reth-p2p-secret.hex");
    if p2p_key.is_file() {
        let normalized: String = fs::read_to_string(&p2p_key)?
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let tmp = keys_dir.join(".reth-p2p-secret.tmp");
        if tmp.exists() {
            fs::remove_file(&tmp)?;
        }
        write_file_mode(&tmp, normalized.as_bytes(), 0o600)?;
        fs::rename(&tmp, &p2p_key)?;
    }
    Ok(())
}

struct TarBudget {
    total_read: u64,
    max_total: u64,
    entry_meta_read: u64,
    max_entry_meta: u64,
    counting_meta: bool,
}

/// Reader wrapper placed *below* `tar::Archive` so every decompressed byte — including tar
/// headers, padding, GNU longname/longlink extensions, PAX extended headers, and directory or
/// file payloads — is counted against the total budget and per-entry metadata cap *before*
/// `tar` can allocate or parse it.
struct BoundedTarReader<'a, R: Read> {
    inner: R,
    budget: Rc<RefCell<TarBudget>>,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for BoundedTarReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "founder tar extraction canceled by signal",
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
            // Allow reading up to 1 byte past the cap so EOF at exact cap succeeds while any
            // byte beyond the cap immediately triggers the budget error without overshooting.
            let max_chunk = limit.saturating_add(1).min(buf.len() as u64) as usize;
            max_chunk.max(1)
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
                    "founder tar decompressed stream exceeds maximum {} bytes",
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
                        "founder tar entry header/metadata exceeds maximum {} bytes",
                        b.max_entry_meta
                    ),
                ));
            }
        }
        Ok(n)
    }
}

/// Non-blocking wrapper around a child process's stdout pipe so stalled subprocesses (e.g. a
/// blocked `gzip -dc`) never prevent `SIGINT`/`SIGTERM` cancellation from terminating the child
/// process group promptly.
struct CancellableChildStdout<'a> {
    stdout: ChildStdout,
    child_pid: u32,
    cancel: &'a AtomicBool,
}

impl<'a> CancellableChildStdout<'a> {
    fn new(stdout: ChildStdout, child_pid: u32, cancel: &'a AtomicBool) -> Result<Self> {
        let flags = fcntl_getfl(&stdout)
            .map_err(io::Error::from)
            .wrap_err("read gzip stdout descriptor flags")?;
        fcntl_setfl(&stdout, flags | OFlags::NONBLOCK)
            .map_err(io::Error::from)
            .wrap_err("set O_NONBLOCK on gzip stdout pipe")?;
        Ok(Self {
            stdout,
            child_pid,
            cancel,
        })
    }
}

impl Read for CancellableChildStdout<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                terminate_process_group(self.child_pid);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "gzip -dc canceled by signal",
                ));
            }
            match self.stdout.read(buf) {
                Ok(n) => return Ok(n),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

fn extract_founder_tar(archive_path: &Path, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    ensure_not_canceled(cancel, "founder tar extraction")?;
    let mut file = File::open(archive_path)
        .wrap_err_with(|| format!("failed to open founder archive: {}", archive_path.display()))?;
    let mut magic = [0_u8; 4];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;

    if n >= 4 && magic == ZSTD_MAGIC {
        let decoder =
            zstd::Decoder::new(file).wrap_err("failed to open zstd founder tar archive")?;
        return extract_tar_stream(decoder, destination, cancel);
    }

    if n >= 2 && magic[..2] == GZIP_MAGIC {
        drop(file);
        let mut cmd = Command::new("gzip");
        cmd.args(["-dc"])
            .arg(archive_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = cmd
            .spawn()
            .wrap_err("failed to spawn gzip -dc for founder tar.gz archive")?;
        let child_pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| eyre::eyre!("gzip stdout unavailable"))?;
        let cancellable_stdout = match CancellableChildStdout::new(stdout, child_pid, cancel) {
            Ok(reader) => reader,
            Err(err) => {
                terminate_process_group(child_pid);
                let _ = child.wait();
                return Err(err);
            }
        };
        let extract_result = extract_tar_stream(cancellable_stdout, destination, cancel);
        if extract_result.is_err() {
            terminate_process_group(child_pid);
            let _ = child.wait();
            return extract_result;
        }
        let status = wait_child_cancellable(&mut child, cancel, "gzip -dc")?;
        ensure!(
            status.success(),
            "gzip -dc failed for {} ({status})",
            archive_path.display()
        );
        return Ok(());
    }

    extract_tar_stream(file, destination, cancel)
}

fn read_magic_prefix<R: Read>(reader: &mut R, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut prefix = Vec::with_capacity(4);
    let mut buf = [0_u8; 4];
    while prefix.len() < 4 {
        ensure_not_canceled(cancel, "founder tar extraction")?;
        let remaining = 4 - prefix.len();
        match reader.read(&mut buf[..remaining]) {
            Ok(0) => break,
            Ok(n) => prefix.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).wrap_err("failed to read founder tar stream header"),
        }
    }
    Ok(prefix)
}

fn extract_founder_tar_reader<R: Read + Send>(
    mut reader: R,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_not_canceled(cancel, "founder tar extraction")?;
    let magic = read_magic_prefix(&mut reader, cancel)?;
    ensure!(!magic.is_empty(), "founder tar stream is empty");
    let chained = io::Cursor::new(magic.clone()).chain(reader);

    if magic.len() >= 4 && magic[..4] == ZSTD_MAGIC {
        let decoder =
            zstd::Decoder::new(chained).wrap_err("failed to open zstd founder tar stream")?;
        return extract_tar_stream(decoder, destination, cancel);
    }

    if magic.len() >= 2 && magic[..2] == GZIP_MAGIC {
        let mut cmd = Command::new("gzip");
        cmd.args(["-dc"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = cmd
            .spawn()
            .wrap_err("failed to spawn gzip -dc for founder tar.gz stream")?;
        let child_pid = child.id();
        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| eyre::eyre!("gzip stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| eyre::eyre!("gzip stdout unavailable"))?;
        let cancellable_stdout = match CancellableChildStdout::new(stdout, child_pid, cancel) {
            Ok(reader) => reader,
            Err(err) => {
                drop(child_stdin);
                terminate_process_group(child_pid);
                let _ = child.wait();
                return Err(err);
            }
        };
        let extract_result = thread::scope(|scope| {
            let feeder = scope.spawn(move || -> io::Result<()> {
                let mut src = chained;
                let mut buf = [0_u8; 16 * 1024];
                let mut total_fed: u64 = 0;
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let n = match src.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => return Err(err),
                    };
                    total_fed = total_fed.saturating_add(n as u64);
                    if total_fed > MAX_FOUNDER_TAR_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "compressed founder tar stream exceeds maximum {MAX_FOUNDER_TAR_BYTES} bytes"
                            ),
                        ));
                    }
                    if let Err(err) = child_stdin.write_all(&buf[..n]) {
                        if err.kind() == io::ErrorKind::BrokenPipe {
                            break;
                        }
                        return Err(err);
                    }
                }
                drop(child_stdin);
                Ok(())
            });
            let res = extract_tar_stream(cancellable_stdout, destination, cancel);
            if res.is_err() {
                terminate_process_group(child_pid);
            }
            let feed_res = feeder
                .join()
                .map_err(|_| eyre::eyre!("gzip stdin feeder thread panicked"))?;
            res?;
            feed_res.wrap_err("failed to stream gzip founder tar to gzip -dc")?;
            Ok(())
        });
        if extract_result.is_err() {
            terminate_process_group(child_pid);
            let _ = child.wait();
            return extract_result;
        }
        let status = wait_child_cancellable(&mut child, cancel, "gzip -dc")?;
        ensure!(
            status.success(),
            "gzip -dc failed on stdin stream ({status})"
        );
        return Ok(());
    }

    extract_tar_stream(chained, destination, cancel)
}

fn extract_tar_stream<R: Read>(reader: R, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    extract_tar_stream_with_limits(
        reader,
        destination,
        MAX_FOUNDER_TAR_BYTES,
        MAX_TAR_HEADER_METADATA_BYTES,
        cancel,
    )
}

fn extract_tar_stream_with_limits<R: Read>(
    reader: R,
    destination: &Path,
    max_total_bytes: u64,
    max_entry_meta_bytes: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    let budget = Rc::new(RefCell::new(TarBudget {
        total_read: 0,
        max_total: max_total_bytes,
        entry_meta_read: 0,
        max_entry_meta: max_entry_meta_bytes,
        counting_meta: true,
    }));
    let bounded = BoundedTarReader {
        inner: reader,
        budget: Rc::clone(&budget),
        cancel,
    };
    let mut archive = tar::Archive::new(bounded);
    let mut entry_count: usize = 0;

    for entry in archive.entries().wrap_err("failed to read founder tar")? {
        let mut entry = entry.wrap_err("failed to read founder tar entry")?;
        ensure_not_canceled(cancel, "founder tar extraction")?;
        entry_count += 1;
        ensure!(
            entry_count <= MAX_FOUNDER_TAR_ENTRIES,
            "founder tar archive exceeds maximum {MAX_FOUNDER_TAR_ENTRIES} entries"
        );

        let entry_type = entry.header().entry_type();
        let raw_path = entry.path()?.into_owned();
        let rel = sanitize_tar_relative_path(&raw_path)?;
        let declared_size = entry.size();
        if rel.as_os_str().is_empty() {
            ensure!(
                declared_size == 0,
                "refusing non-zero payload size ({declared_size} bytes) on root entry in founder tar"
            );
            let mut b = budget.borrow_mut();
            b.counting_meta = true;
            b.entry_meta_read = 0;
            continue;
        }
        let target = destination.join(&rel);
        let mode = entry.header().mode()? & 0o7777;

        if entry_type.is_dir() {
            ensure!(
                declared_size == 0,
                "refusing directory entry with non-zero payload size ({declared_size} bytes) in founder tar: {}",
                raw_path.display()
            );
            ensure!(
                mode & 0o6000 == 0,
                "refusing setuid/setgid directory in founder tar: {}",
                raw_path.display()
            );
            fs::create_dir_all(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777))?;
            let mut b = budget.borrow_mut();
            b.counting_meta = true;
            b.entry_meta_read = 0;
            continue;
        }

        ensure!(
            entry_type.is_file(),
            "refusing non-regular entry ({entry_type:?}) in founder tar: {}",
            raw_path.display()
        );
        ensure!(
            mode & 0o111 == 0,
            "refusing executable file (mode {mode:04o}) in founder tar: {}",
            raw_path.display()
        );
        ensure!(
            mode & 0o6000 == 0,
            "refusing setuid/setgid file (mode {mode:04o}) in founder tar: {}",
            raw_path.display()
        );
        ensure!(
            declared_size <= max_total_bytes,
            "founder tar entry exceeds maximum {max_total_bytes} bytes: {}",
            raw_path.display()
        );

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let file_mode = mode & 0o777;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(file_mode)
            .open(&target)
            .wrap_err_with(|| format!("failed to extract {}", raw_path.display()))?;

        // Switch off per-entry header metadata cap while streaming regular file payload; the
        // total stream budget remains enforced on every read.
        budget.borrow_mut().counting_meta = false;
        let copied = io::copy(&mut entry.by_ref().take(declared_size), &mut out)
            .wrap_err_with(|| format!("failed to extract {}", raw_path.display()))?;
        {
            let mut b = budget.borrow_mut();
            b.counting_meta = true;
            b.entry_meta_read = 0;
        }

        ensure!(
            copied == declared_size,
            "truncated entry in founder tar: {}",
            raw_path.display()
        );
        out.sync_all()?;
        fs::set_permissions(&target, fs::Permissions::from_mode(file_mode))?;
    }
    Ok(())
}

fn sanitize_tar_relative_path(raw: &Path) -> Result<PathBuf> {
    let mut clean = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => clean.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe path in founder tar archive: {}", raw.display());
            }
        }
    }
    Ok(clean)
}

fn resolve_extracted_founder_root(extract_root: &Path) -> Result<PathBuf> {
    if extract_root.join("genesis.json").is_file() {
        return Ok(extract_root.to_path_buf());
    }
    let entries: Vec<PathBuf> = fs::read_dir(extract_root)?
        .map(|entry| Ok(entry?.path()))
        .collect::<Result<_>>()?;
    if entries.len() == 1 && entries[0].is_dir() && entries[0].join("genesis.json").is_file() {
        return Ok(entries[0].clone());
    }
    bail!(
        "founder tar archive must contain genesis.json at root or inside a single top-level directory"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::{self, BundleInput};

    fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn build_fake_operator_bundle_with_script(
        dir: &Path,
        chain_id: u64,
        verify_body: &str,
    ) -> PathBuf {
        let src = dir.join("bundle-src");
        let genesis = src.join("genesis.json");
        write_mode(
            &genesis,
            format!(r#"{{"config":{{"chainId":{chain_id}}}}}"#).as_bytes(),
            0o644,
        );
        let crs = src.join("bn254_g1.dat");
        write_mode(&crs, b"fake-crs-bytes", 0o644);
        let keygen = src.join("outbe-keygen");
        let script = format!(
            r#"#!/bin/sh
set -eu
cmd="$1"
shift
if [ "$cmd" = "verify" ]; then
{verify_body}
fi
if [ "$cmd" = "validator" ]; then
    out=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--output-dir" ]; then
            out="$2"
            shift 2
        else
            shift 2
        fi
    done
    umask 077
    mkdir -p "$out/radicle/keys"
    printf '%s' '1111111111111111111111111111111111111111111111111111111111111111' > "$out/signing-key.hex"
    printf '%s' '2222222222222222222222222222222222222222222222222222222222222222' > "$out/evm-key.hex"
    printf '%s' '3333333333333333333333333333333333333333333333333333333333333333' > "$out/feeder-key.hex"
    printf '%s' '4444444444444444444444444444444444444444444444444444444444444444' > "$out/hyperlane-key.hex"
    printf '%s\n' '5555555555555555555555555555555555555555555555555555555555555555' > "$out/reth-p2p-secret.hex"
    printf '%s' 'fake-radicle-ssh-key' > "$out/radicle/keys/radicle"
    exit 0
fi
exit 1
"#
        );
        write_mode(&keygen, script.as_bytes(), 0o755);
        let dummy_bin = src.join("outbe-chain");
        write_mode(&dummy_bin, b"#!/bin/sh\nexit 0\n", 0o755);

        let bundle_path = dir.join("operator-bundle.tar.zst");
        bundle::create_archive(
            &bundle_path,
            vec![
                BundleInput {
                    path: "genesis.json".into(),
                    source: genesis,
                    mode: 0o644,
                },
                BundleInput {
                    path: ".bb-crs/bn254_g1.dat".into(),
                    source: crs,
                    mode: 0o644,
                },
                BundleInput {
                    path: "outbe-keygen".into(),
                    source: keygen,
                    mode: 0o755,
                },
                BundleInput {
                    path: "outbe-chain".into(),
                    source: dummy_bin,
                    mode: 0o755,
                },
            ],
        )
        .unwrap();
        bundle_path
    }

    fn build_fake_operator_bundle(dir: &Path, chain_id: u64) -> PathBuf {
        // Default verify script enforces 64 hex chars on signing-key.hex (rejects prose like
        // "not-a-BLS-private-key").
        build_fake_operator_bundle_with_script(
            dir,
            chain_id,
            r#"    key_path=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--key" ]; then
            key_path="$2"
            shift 2
        else
            shift 1
        fi
    done
    val="$(tr -d '\r\n' < "$key_path")"
    case "$val" in
        *[!0-9a-fA-F]*|"")
            echo "SECRET_STDOUT_LEAK_MARKER"
            echo "SECRET_STDERR_LEAK_MARKER" >&2
            exit 1
            ;;
    esac
    [ "${#val}" -eq 64 ] || exit 1
    exit 0"#,
        )
    }

    fn write_joining_config(path: &Path, chain_id: u64) {
        let peer_key = "a".repeat(96);
        let yaml = format!(
            r#"version: 1
network:
  chain_id: {chain_id}
  certified_rpc: "http://10.77.0.10:8545"
  advertise_ip: 10.77.0.20
  rpc_bind_ip: 10.77.0.20
  vm_prefix: 24
  vm_gateway: 10.77.0.1
  vm_dns: [1.1.1.1, 8.8.8.8]
  bootnodes:
    - "enode://abcd@10.77.0.10:30303"
  consensus_peers:
    - "{peer_key}@10.77.0.10:30400"
  use_local_defaults: true
feeder:
  public_toml: |
    [oracle]
    sources = "local"
hyperlane:
  public_url: "http://10.77.0.20:9000"
  gasless: true
  origins:
    - domain: {chain_id}
      rpc_urls: ["http://127.0.0.1:8545"]
      rpc_consensus: single
      reorg_period: 0
"#
        );
        fs::write(path, yaml).unwrap();
    }

    fn write_full_node_config(path: &Path, chain_id: u64) {
        let yaml = format!(
            r#"version: 1
network:
  chain_id: {chain_id}
  certified_rpc: "http://10.77.0.10:8545"
  advertise_ip: 10.77.0.21
  rpc_bind_ip: 10.77.0.21
  vm_prefix: 24
  vm_gateway: 10.77.0.1
  vm_dns: [1.1.1.1, 8.8.8.8]
  bootnodes:
    - "enode://abcd@10.77.0.10:30303"
  use_local_defaults: true
"#
        );
        fs::write(path, yaml).unwrap();
    }

    fn write_valid_keys_dir(keys_dir: &Path) {
        for (rel, ch) in [
            ("signing-key.hex", "1"),
            ("evm-key.hex", "2"),
            ("feeder-key.hex", "3"),
            ("hyperlane-key.hex", "4"),
            ("reth-p2p-secret.hex", "5"),
        ] {
            write_mode(&keys_dir.join(rel), ch.repeat(64).as_bytes(), 0o600);
        }
        write_mode(
            &keys_dir.join("radicle/keys/radicle"),
            b"fake-radicle-ssh-key",
            0o600,
        );
    }

    #[test]
    fn joining_validator_stages_keys_marker_configs_and_strips_bundle_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = build_fake_operator_bundle(tmp.path(), 707);
        let cfg = tmp.path().join("operator.yaml");
        write_joining_config(&cfg, 707);

        let staged = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();
        assert_eq!(staged.role, NodeRole::JoiningValidator);
        assert!(staged.key_verified);
        assert!(staged.validator_address.is_some());
        assert!(staged.hyperlane_address.is_some());

        let tree = staged.tree_path();
        assert!(fs::read_to_string(tree.join("validator.env"))
            .unwrap()
            .contains("OUTBE_NODE_ROLE=\"joining-validator\"\n"));
        assert!(!tree.join(crate::NODE_ROLE_FILE).exists());
        // Bundle executables MUST NOT be copied onto the noexec VM data disk.
        assert!(!tree.join("outbe-keygen").exists());
        assert!(!tree.join("outbe-chain").exists());
        assert!(!tree.join("operator-manifest.json").exists());

        // Secret permissions are strictly 0600 and reth-p2p-secret.hex has no trailing newline.
        assert_eq!(
            fs::metadata(tree.join("keys/evm-key.hex"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_to_string(tree.join("keys/reth-p2p-secret.hex"))
                .unwrap()
                .len(),
            64
        );
    }

    #[test]
    fn keys_from_enforces_strict_allowlist_and_rejects_extra_files_or_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = build_fake_operator_bundle(tmp.path(), 707);
        let cfg = tmp.path().join("operator.yaml");
        write_joining_config(&cfg, 707);

        // 1. Exact 6 secrets + optional radicle.pub passes.
        let valid_keys = tmp.path().join("valid-keys");
        write_valid_keys_dir(&valid_keys);
        write_mode(
            &valid_keys.join("radicle/keys/radicle.pub"),
            b"ssh-ed25519 AAAA test",
            0o644,
        );
        let staged =
            prepare_joining_validator_tree(&bundle, &cfg, Some(&valid_keys), tmp.path()).unwrap();
        assert!(staged
            .tree_path()
            .join("keys/radicle/keys/radicle.pub")
            .is_file());

        // 2. Extra unrelated file in --keys-from is refused.
        let extra_file_keys = tmp.path().join("extra-file-keys");
        write_valid_keys_dir(&extra_file_keys);
        write_mode(
            &extra_file_keys.join("unrelated-backup.env"),
            b"AWS_SECRET=oops",
            0o600,
        );
        let err = prepare_joining_validator_tree(&bundle, &cfg, Some(&extra_file_keys), tmp.path())
            .err()
            .expect("--keys-from with extra file must fail");
        assert!(
            err.to_string()
                .contains("refusing unexpected file in --keys-from: unrelated-backup.env"),
            "{err}"
        );

        // 3. Extra unrelated directory in --keys-from is refused.
        let extra_dir_keys = tmp.path().join("extra-dir-keys");
        write_valid_keys_dir(&extra_dir_keys);
        fs::create_dir_all(extra_dir_keys.join("other-wallet")).unwrap();
        let err = prepare_joining_validator_tree(&bundle, &cfg, Some(&extra_dir_keys), tmp.path())
            .err()
            .expect("--keys-from with extra dir must fail");
        assert!(
            err.to_string()
                .contains("refusing unexpected directory in --keys-from: other-wallet"),
            "{err}"
        );
    }

    #[test]
    fn full_node_stages_zero_keys_and_rejects_keys_from_or_validator_config() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = build_fake_operator_bundle(tmp.path(), 707);
        let full_cfg = tmp.path().join("fullnode.yaml");
        write_full_node_config(&full_cfg, 707);

        let staged = prepare_full_node_tree(&bundle, &full_cfg, None, tmp.path()).unwrap();
        assert_eq!(staged.role, NodeRole::FullNode);
        assert!(staged.validator_address.is_none());
        assert!(staged.hyperlane_address.is_none());

        let tree = staged.tree_path();
        assert!(!tree.join(crate::NODE_ROLE_FILE).exists());
        assert!(tree.join("genesis.json").is_file());
        assert!(tree.join(".bb-crs/bn254_g1.dat").is_file());
        assert!(tree.join("validator.env").is_file());
        assert!(!tree.join("keys").exists());
        assert!(!tree.join("feeder-public.toml").exists());
        assert!(!tree.join("hyperlane-public.toml").exists());
        assert!(!tree.join("outbe-chain").exists());

        let env_text = fs::read_to_string(tree.join("validator.env")).unwrap();
        assert!(env_text.contains("OUTBE_NODE_ROLE=\"full-node\"\n"));
        assert!(!env_text.contains("OUTBE_VALIDATOR_ADDRESS"));
        assert!(!env_text.contains("OUTBE_CONSENSUS_PEERS"));

        // Passing --keys-from for full-node must be refused.
        let err = prepare_full_node_tree(&bundle, &full_cfg, Some(tmp.path()), tmp.path())
            .err()
            .expect("full-node must refuse --keys-from");
        assert!(err.to_string().contains("zero keys"), "{err}");

        // Passing an OperatorConfig with feeder/hyperlane/consensus_peers must be refused.
        let op_cfg = tmp.path().join("operator.yaml");
        write_joining_config(&op_cfg, 707);
        let err = prepare_full_node_tree(&bundle, &op_cfg, None, tmp.path())
            .err()
            .expect("full-node must reject validator sections");
        assert!(
            format!("{err:#}").contains("unknown field"),
            "expected deny_unknown_fields error, got: {err:#}"
        );
    }

    #[test]
    fn founder_requires_verified_keygen_or_explicit_skip_and_rejects_bad_bls_key() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = build_fake_operator_bundle(tmp.path(), 707);
        let cfg = tmp.path().join("operator.yaml");
        write_joining_config(&cfg, 707);
        let joining = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();

        // 1. Standalone founder dir without sibling bundle fails closed under Auto.
        let standalone = tmp.path().join("standalone-founder");
        copy_dir_plain(joining.tree_path(), &standalone);
        rewrite_env_role(&standalone, "founder");
        let err = prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Auto)
            .err()
            .expect("founder without bundle must refuse unless --skip-key-verify");
        assert!(
            err.to_string()
                .contains("no verified operator bundle found"),
            "{err}"
        );

        // 2. Explicit Skip succeeds and reports key_verified == false.
        let skipped =
            prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Skip).unwrap();
        assert!(!skipped.key_verified);

        // 3. Explicit Bundle runs real outbe-keygen verify; corrupt signing-key.hex fails without
        // leaking keygen stdout/stderr.
        let staged_ok =
            prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Bundle(&bundle))
                .unwrap();
        assert!(staged_ok.key_verified);

        write_mode(
            &standalone.join("keys/signing-key.hex"),
            b"not-a-BLS-private-key",
            0o600,
        );
        let err = prepare_founder_tree(&standalone, tmp.path(), FounderKeyVerify::Bundle(&bundle))
            .err()
            .expect("corrupt founder signing-key.hex must fail verification");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("signing-key.hex failed outbe-keygen verify"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("SECRET_STDOUT_LEAK_MARKER")
                && !rendered.contains("SECRET_STDERR_LEAK_MARKER"),
            "verify failure must never leak stdout/stderr: {rendered}"
        );

        // 4. Auto-discovery finds <artifacts>/joining/outbe-validator-operator-bundle.tar.zst.
        let artifacts = tmp.path().join("net-artifacts");
        let vm_founder = artifacts.join("vm/validator-0");
        copy_dir_plain(joining.tree_path(), &vm_founder);
        rewrite_env_role(&vm_founder, "founder");
        fs::create_dir_all(artifacts.join("joining")).unwrap();
        fs::copy(
            &bundle,
            artifacts.join("joining/outbe-validator-operator-bundle.tar.zst"),
        )
        .unwrap();
        let auto_staged =
            prepare_founder_tree(&vm_founder, tmp.path(), FounderKeyVerify::Auto).unwrap();
        assert!(auto_staged.key_verified);
        assert!(auto_staged
            .keygen_source
            .as_deref()
            .unwrap_or_default()
            .starts_with("artifacts:"));
    }

    #[test]
    fn founder_tar_rejects_traversal_and_extracts_wrapped_valid_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = build_fake_operator_bundle(tmp.path(), 707);
        let cfg = tmp.path().join("operator.yaml");
        write_joining_config(&cfg, 707);
        let joining = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();
        let founder_dir = tmp.path().join("validator-0");
        copy_dir_plain(joining.tree_path(), &founder_dir);
        rewrite_env_role(&founder_dir, "founder");

        // Pack into a .tar.zst wrapped in `validator-0/`.
        let tar_zst = tmp.path().join("validator-0.tar.zst");
        {
            let file = File::create(&tar_zst).unwrap();
            let mut enc = zstd::Encoder::new(file, 3).unwrap();
            {
                let mut builder = tar::Builder::new(&mut enc);
                builder.append_dir_all("validator-0", &founder_dir).unwrap();
                builder.finish().unwrap();
            }
            enc.finish().unwrap();
        }
        let staged =
            prepare_founder_tree(&tar_zst, tmp.path(), FounderKeyVerify::Bundle(&bundle)).unwrap();
        assert_eq!(staged.role, NodeRole::Founder);
        assert!(staged.key_verified);
        assert!(staged.tree_path().join("genesis.json").is_file());
        assert!(!staged.tree_path().join(crate::NODE_ROLE_FILE).exists());

        // Tar with an executable file is rejected.
        let bad_tar = tmp.path().join("bad-exec.tar");
        {
            let file = File::create(&bad_tar).unwrap();
            let mut builder = tar::Builder::new(file);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o755);
            header.set_size(4);
            header.set_cksum();
            builder
                .append_data(&mut header, "evil.sh", &b"#!/\n"[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let err = prepare_founder_tree(&bad_tar, tmp.path(), FounderKeyVerify::Skip)
            .err()
            .expect("tar with executable must fail");
        assert!(err.to_string().contains("refusing executable"), "{err}");
    }

    #[test]
    fn founder_tar_enforces_decompressed_stream_and_header_metadata_budgets() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);

        // 1. Directory entry with non-zero declared payload size (e.g. a 600 MiB dir bomb) is
        // rejected immediately without allocating or skipping the payload.
        let dir_bomb_tar = tmp.path().join("dir-bomb.tar");
        {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(600 * 1024 * 1024);
            header.set_path("bomb-dir").unwrap();
            header.set_cksum();
            let mut f = File::create(&dir_bomb_tar).unwrap();
            f.write_all(header.as_bytes()).unwrap();
            f.write_all(&[0u8; 1024]).unwrap();
        }
        let err = extract_founder_tar(&dir_bomb_tar, &tmp.path().join("out-dir"), &cancel)
            .expect_err("directory with non-zero payload must be rejected");
        assert!(
            err.to_string()
                .contains("directory entry with non-zero payload size"),
            "{err}"
        );

        // 2. Oversized PAX extended header (> 1 MiB) is rejected BEFORE tar allocates it.
        let pax_bomb_zst = tmp.path().join("pax-bomb.tar.zst");
        {
            let file = File::create(&pax_bomb_zst).unwrap();
            let mut enc = zstd::Encoder::new(file, 3).unwrap();
            let pax_size: u64 = 2 * 1024 * 1024;
            let mut pax_header = tar::Header::new_ustar();
            pax_header.set_entry_type(tar::EntryType::XHeader);
            pax_header.set_mode(0o644);
            pax_header.set_size(pax_size);
            pax_header.set_path("PaxHeaders.0/huge").unwrap();
            pax_header.set_cksum();
            enc.write_all(pax_header.as_bytes()).unwrap();
            let chunk = vec![b'a'; 64 * 1024];
            let mut rem = pax_size as usize;
            while rem > 0 {
                let n = rem.min(chunk.len());
                enc.write_all(&chunk[..n]).unwrap();
                rem -= n;
            }
            enc.finish().unwrap();
        }
        let err = extract_founder_tar(&pax_bomb_zst, &tmp.path().join("out-pax"), &cancel)
            .expect_err("oversized PAX header must fail before allocation");
        assert!(
            format!("{err:#}").contains("entry header/metadata exceeds maximum"),
            "{err:#}"
        );

        // 3. Total decompressed stream cap counts headers + padding + payloads (even small files
        // whose headers alone push total decompressed bytes past the budget).
        let stream_tar = tmp.path().join("stream-over.tar");
        {
            let file = File::create(&stream_tar).unwrap();
            let mut builder = tar::Builder::new(file);
            for i in 0..8 {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                h.set_size(1);
                h.set_cksum();
                builder
                    .append_data(&mut h, format!("f{i}.txt"), &b"x"[..])
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        // 8 entries * 1024 bytes (header + padded block) + 1024 EOF > 4096 bytes, even though
        // file payload bytes total only 8 bytes!
        let f = File::open(&stream_tar).unwrap();
        let err = extract_tar_stream_with_limits(
            f,
            &tmp.path().join("out-stream"),
            4096,
            MAX_TAR_HEADER_METADATA_BYTES,
            &cancel,
        )
        .expect_err("decompressed tar headers + padding exceeding total cap must fail");
        assert!(
            format!("{err:#}").contains("decompressed stream exceeds maximum"),
            "{err:#}"
        );
    }

    #[test]
    fn cleanup_stale_staging_dirs_removes_only_matching_non_symlink_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let stale_a = tmp.path().join(".outbe-prepare-123");
        let stale_b = tmp.path().join(".outbe-disk-founder-456");
        let stale_c = tmp.path().join(".outbe-vm-disk-789");
        let keep_dir = tmp.path().join("unrelated-dir");
        let keep_file = tmp.path().join(".outbe-disk-regular-file");
        let outside_dir = tmp.path().join("outside-target");
        let symlink_dir = tmp.path().join(".outbe-disk-symlink");

        for d in [&stale_a, &stale_b, &stale_c, &keep_dir, &outside_dir] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(&keep_file, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside_dir, &symlink_dir).unwrap();

        let live_locked = create_locked_staging_dir(tmp.path(), ".outbe-disk-live-").unwrap();
        let live_path = live_locked.path().to_path_buf();
        fs::write(live_path.join("in-flight.hex"), b"live").unwrap();

        let removed = cleanup_stale_staging_dirs(tmp.path()).unwrap();
        assert_eq!(removed.len(), 3);
        assert!(!stale_a.exists());
        assert!(!stale_b.exists());
        assert!(!stale_c.exists());
        assert!(
            live_path.is_dir() && live_path.join("in-flight.hex").is_file(),
            "cleanup_stale_staging_dirs must NEVER delete a live locked staging dir"
        );
        assert!(keep_dir.is_dir());
        assert!(keep_file.is_file());
        assert!(outside_dir.is_dir());
        assert!(fs::symlink_metadata(&symlink_dir).is_ok());
        drop(live_locked);
    }

    #[test]
    fn founder_from_reader_accepts_plain_zstd_and_gzip_tar_and_rejects_attacks() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = build_fake_operator_bundle(tmp.path(), 707);
        let cfg = tmp.path().join("operator.yaml");
        write_joining_config(&cfg, 707);
        let joining = prepare_joining_validator_tree(&bundle, &cfg, None, tmp.path()).unwrap();
        let founder_dir = tmp.path().join("founder-src");
        copy_dir_plain(joining.tree_path(), &founder_dir);
        rewrite_env_role(&founder_dir, "founder");

        // Build plain tar in memory.
        let mut plain_tar = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut plain_tar);
            builder.append_dir_all(".", &founder_dir).unwrap();
            builder.finish().unwrap();
        }

        // 1. Auto key-verify is refused on stdin streams.
        let err = prepare_founder_tree_from_reader(
            io::Cursor::new(plain_tar.clone()),
            tmp.path(),
            FounderKeyVerify::Auto,
        )
        .err()
        .expect("stdin tar with Auto key-verify must fail closed");
        assert!(
            err.to_string()
                .contains("cannot auto-discover operator bundle"),
            "{err}"
        );

        // 2. Plain tar succeeds with verified operator bundle.
        let staged_plain = prepare_founder_tree_from_reader(
            io::Cursor::new(plain_tar.clone()),
            tmp.path(),
            FounderKeyVerify::Bundle(&bundle),
        )
        .unwrap();
        assert_eq!(staged_plain.role, NodeRole::Founder);
        assert!(staged_plain.key_verified);
        assert!(staged_plain.tree_path().join("genesis.json").is_file());
        drop(staged_plain);

        // 3. Zstd-compressed tar stream succeeds.
        let zstd_tar = zstd::stream::encode_all(io::Cursor::new(&plain_tar), 1).unwrap();
        let staged_zstd = prepare_founder_tree_from_reader(
            io::Cursor::new(zstd_tar),
            tmp.path(),
            FounderKeyVerify::Bundle(&bundle),
        )
        .unwrap();
        assert!(staged_zstd.key_verified);
        drop(staged_zstd);

        // 4. Gzip-compressed tar stream succeeds.
        let mut gzip_child = Command::new("gzip")
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        gzip_child
            .stdin
            .take()
            .unwrap()
            .write_all(&plain_tar)
            .unwrap();
        let gzip_out = gzip_child.wait_with_output().unwrap();
        assert!(gzip_out.status.success());
        let staged_gz = prepare_founder_tree_from_reader(
            io::Cursor::new(gzip_out.stdout),
            tmp.path(),
            FounderKeyVerify::Bundle(&bundle),
        )
        .unwrap();
        assert!(staged_gz.key_verified);
        drop(staged_gz);

        // 5. Reject path traversal (`..`), symlinks, and executable files in streamed tar.
        let mut evil_traversal = Vec::new();
        {
            let mut h = tar::Header::new_gnu();
            h.set_size(4);
            h.set_mode(0o600);
            h.set_entry_type(tar::EntryType::Regular);
            h.as_gnu_mut().unwrap().name[..10].copy_from_slice(b"../escape\0");
            h.set_cksum();
            evil_traversal.extend_from_slice(h.as_bytes());
            let mut block = [0u8; 512];
            block[..4].copy_from_slice(b"evil");
            evil_traversal.extend_from_slice(&block);
            evil_traversal.extend_from_slice(&[0u8; 1024]);
        }
        let err = prepare_founder_tree_from_reader(
            io::Cursor::new(evil_traversal),
            tmp.path(),
            FounderKeyVerify::Skip,
        )
        .err()
        .expect("traversal tar stream must fail");
        assert!(err.to_string().contains("unsafe path"), "{err}");

        let mut evil_symlink = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut evil_symlink);
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            builder
                .append_link(&mut h, "keys/link", "/etc/passwd")
                .unwrap();
            builder.finish().unwrap();
        }
        let err = prepare_founder_tree_from_reader(
            io::Cursor::new(evil_symlink),
            tmp.path(),
            FounderKeyVerify::Skip,
        )
        .err()
        .expect("symlink tar stream must fail");
        assert!(err.to_string().contains("non-regular entry"), "{err}");

        let mut evil_exec = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut evil_exec);
            let mut h = tar::Header::new_gnu();
            h.set_size(4);
            h.set_mode(0o755);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            builder
                .append_data(&mut h, "run.sh", &b"#!/\n"[..])
                .unwrap();
            builder.finish().unwrap();
        }
        let err = prepare_founder_tree_from_reader(
            io::Cursor::new(evil_exec),
            tmp.path(),
            FounderKeyVerify::Skip,
        )
        .err()
        .expect("executable tar stream must fail");
        assert!(err.to_string().contains("executable file"), "{err}");

        // 6. Empty stream fails cleanly.
        let err = prepare_founder_tree_from_reader(
            io::Cursor::new(Vec::<u8>::new()),
            tmp.path(),
            FounderKeyVerify::Skip,
        )
        .err()
        .expect("empty tar stream must fail");
        assert!(err.to_string().contains("empty"), "{err}");

        // 7. Joining-validator tar stream passes `prepare_joining_validator_tree_from_reader`
        //    and rejects a founder tar (missing `node-role: joining-validator`).
        let mut joining_tar = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut joining_tar);
            builder.append_dir_all(".", joining.tree_path()).unwrap();
            builder.finish().unwrap();
        }
        let staged_join = prepare_joining_validator_tree_from_reader(
            io::Cursor::new(joining_tar),
            tmp.path(),
            FounderKeyVerify::Bundle(&bundle),
        )
        .unwrap();
        assert_eq!(staged_join.role, NodeRole::JoiningValidator);
        assert!(staged_join.key_verified);
        assert!(
            fs::read_to_string(staged_join.tree_path().join("validator.env"))
                .unwrap()
                .contains("OUTBE_NODE_ROLE=\"joining-validator\"\n")
        );
        drop(staged_join);

        let err = prepare_joining_validator_tree_from_reader(
            io::Cursor::new(plain_tar),
            tmp.path(),
            FounderKeyVerify::Bundle(&bundle),
        )
        .err()
        .expect("founder tar must be rejected by joining-validator reader");
        assert!(err.to_string().contains("role mismatch"), "{err}");
    }

    fn rewrite_env_role(tree: &Path, role: &str) {
        let env_path = tree.join("validator.env");
        let raw = fs::read_to_string(&env_path).unwrap();
        let replaced = raw.replace(
            "OUTBE_NODE_ROLE=\"joining-validator\"",
            &format!("OUTBE_NODE_ROLE=\"{role}\""),
        );
        fs::write(&env_path, replaced).unwrap();
        fs::set_permissions(&env_path, fs::Permissions::from_mode(0o640)).unwrap();
    }

    fn copy_dir_plain(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in WalkDir::new(src).follow_links(false) {
            let entry = entry.unwrap();
            let rel = entry.path().strip_prefix(src).unwrap();
            if rel.as_os_str().is_empty() {
                continue;
            }
            let target = dst.join(rel);
            let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
            } else {
                fs::copy(entry.path(), &target).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
            }
        }
    }
}
