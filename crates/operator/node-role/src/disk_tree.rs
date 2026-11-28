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

pub(crate) mod child;
mod founder_tar;
mod keys;
mod staging;
mod tar_extract;
#[cfg(test)]
mod tests;

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use alloy_primitives::Address;
use eyre::{bail, ensure, Context, Result};

pub use self::staging::cleanup_stale_staging_dirs;
use self::{
    founder_tar::{
        extract_founder_tar, extract_founder_tar_reader, resolve_extracted_founder_root,
    },
    keys::{copy_keys_strict, generate_keys_with_bundle_keygen, normalize_keys_tree},
    staging::{create_locked_staging_dir, LockedTempDir},
};
use crate::{
    bundle,
    config::{FullNodeConfig, NodeRole, OperatorConfig},
    validator_address, vm_boot,
};

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
        "reth.toml",
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
    fs::set_permissions(tree.join("reth.toml"), fs::Permissions::from_mode(0o640))?;

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
