//! Validator key staging: generation by the bundle `outbe-keygen`, strict copy, and permission
//! normalization.

use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Component, Path},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};

use eyre::{bail, ensure, Context, Result};
use walkdir::WalkDir;

use super::{
    child::wait_child_cancellable, copy_file_mode, create_dir_mode, ensure_not_canceled,
    write_file_mode,
};
use crate::path_text;

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

pub(super) fn generate_keys_with_bundle_keygen(
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

pub(super) fn copy_keys_strict(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
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

pub(super) fn normalize_keys_tree(keys_dir: &Path) -> Result<()> {
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
