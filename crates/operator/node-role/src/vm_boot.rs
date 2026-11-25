//! Verify the unmeasured VM data disk and prepare runtime directories before daemons start.

use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Component, Path},
};

use eyre::{bail, ensure, Context, Result};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{
        fchmod, fchown, fstat, mkdirat, openat, statat, unlinkat, AtFlags, FileType, Mode, OFlags,
        CWD,
    },
    io::Errno,
    process::{Gid, Uid},
};
use walkdir::WalkDir;

use crate::{
    config::{NodeRole, NODE_ROLE_FILE},
    install, process,
};

const DEFAULT_VM_BIN_DIR: &str = "/usr/local/bin";
const REQUIRED_MOUNT_OPTIONS: &[&str] = &["noexec", "nosuid", "nodev"];

const STALE_SOCKETS: &[&str] = &[
    "keys/radicle/node/outbe-control.sock",
    "validator-0/data/reth.ipc",
];

/// Large service datadir skipped during recursive walk; kernel mount flags enforce safety inside.
const SKIPPED_STATE_SUBTREE: &str = "validator-0/data";

/// Service-state subtrees where daemons write runtime files (sockets, fifos, exec bits allowed).
const SERVICE_STATE_ROOTS: &[&str] = &[
    "validator-0",
    "state",
    "keys/radicle/storage",
    "keys/radicle/node",
    "keys/radicle/cobs",
];

const VALIDATOR_DIRS: &[&str] = &[
    "validator-0",
    "validator-0/data",
    "validator-0/consensus",
    "validator-0/logs",
];

const RADICLE_DIRS: &[&str] = &[
    "keys/radicle/storage",
    "keys/radicle/node",
    "keys/radicle/cobs",
];

const CONFIG_FILES: &[&str] = &[
    "validator.env",
    "feeder-public.toml",
    "hyperlane-public.toml",
];

struct BootOptions<'a> {
    bin_dir: Option<&'a Path>,
    require_root_owner: bool,
    apply_ownership: bool,
    prepare_runtime_dirs: bool,
    expected_role: Option<NodeRole>,
}

/// Read and validate `/opt/outbe-chain/node-role` (absent defaults to [`NodeRole::Founder`]).
pub fn read_role_marker(root: &Path, require_root_owner: bool) -> Result<NodeRole> {
    let path = root.join(NODE_ROLE_FILE);
    match fs::symlink_metadata(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(NodeRole::Founder),
        Err(err) => Err(err).wrap_err_with(|| format!("failed to stat {}", path.display())),
        Ok(_) => {
            verify_config_file(&path, require_root_owner)?;
            let raw = fs::read_to_string(&path)
                .wrap_err_with(|| format!("failed to read {}", path.display()))?;
            NodeRole::parse_marker(&raw)
                .wrap_err_with(|| format!("invalid role marker {}", path.display()))
        }
    }
}

/// Verify an emitted VM data tree without checking live mountinfo or changing ownership.
pub fn verify_tree_without_mount(root: &Path, bin_dir: &Path) -> Result<()> {
    let mountinfo = format!(
        "100 1 8:1 / {} rw,nosuid,nodev,noexec - ext4 /dev/vda1 rw\n",
        root.display()
    );
    boot_root(
        root,
        &mountinfo,
        &BootOptions {
            bin_dir: Some(bin_dir),
            require_root_owner: false,
            apply_ownership: false,
            prepare_runtime_dirs: true,
            expected_role: None,
        },
    )
}

/// Strictly verify a static VM data tree for `expected_role` without mutating `root` or checking
/// live `/proc/self/mountinfo`.
pub fn verify_static_tree_for_role(
    root: &Path,
    expected_role: NodeRole,
    bin_dir: Option<&Path>,
) -> Result<()> {
    let mountinfo = format!(
        "100 1 8:1 / {} rw,nosuid,nodev,noexec - ext4 /dev/vda1 rw\n",
        root.display()
    );
    boot_root(
        root,
        &mountinfo,
        &BootOptions {
            bin_dir,
            require_root_owner: false,
            apply_ownership: false,
            prepare_runtime_dirs: false,
            expected_role: Some(expected_role),
        },
    )
}

/// Verify `/opt/outbe-chain` and prepare runtime directories during VM boot.
pub fn run(bin_dir: Option<&Path>) -> Result<()> {
    process::require_root()?;
    let root = Path::new(process::ROOT);
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .wrap_err("failed to read /proc/self/mountinfo")?;
    let bin_dir = bin_dir.unwrap_or_else(|| Path::new(DEFAULT_VM_BIN_DIR));
    boot_root(
        root,
        &mountinfo,
        &BootOptions {
            bin_dir: Some(bin_dir),
            require_root_owner: true,
            apply_ownership: true,
            prepare_runtime_dirs: true,
            expected_role: None,
        },
    )
}

fn boot_root(root: &Path, mountinfo: &str, options: &BootOptions<'_>) -> Result<()> {
    verify_mount(root, mountinfo)?;
    verify_tree(root)?;

    let role = read_role_marker(root, options.require_root_owner)?;
    if let Some(expected) = options.expected_role {
        ensure!(
            role == expected,
            "VM data disk role mismatch: expected {expected}, found {role}"
        );
    }

    let env_path = root.join("validator.env");
    match role {
        NodeRole::Founder | NodeRole::JoiningValidator => {
            for name in CONFIG_FILES {
                verify_config_file(&root.join(name), options.require_root_owner)?;
            }

            let environment = install::read_environment_for_role(&env_path, role)?;
            install::validate_environment_for_role(&environment, role)?;

            install::required_file(&root.join("genesis.json"))?;
            install::validate_genesis(root, &environment["OUTBE_CHAIN_ID"])?;
            let chain_id: u64 = environment["OUTBE_CHAIN_ID"]
                .parse()
                .wrap_err("OUTBE_CHAIN_ID must be a positive integer")?;
            let hyperlane_public = fs::read_to_string(root.join("hyperlane-public.toml"))
                .wrap_err("failed to read hyperlane-public.toml")?;
            crate::config::validate_hyperlane_public_toml(&hyperlane_public, chain_id, true)?;
            install::required_file(&root.join(".bb-crs/bn254_g1.dat"))?;

            verify_secrets(root, options.bin_dir)?;
        }
        NodeRole::FullNode => {
            verify_config_file(&env_path, options.require_root_owner)?;
            for forbidden in ["feeder-public.toml", "hyperlane-public.toml", "keys"] {
                let path = root.join(forbidden);
                ensure!(
                    fs::symlink_metadata(&path).is_err(),
                    "full-node data disk must not contain validator artifact: {}",
                    path.display()
                );
            }

            let environment = install::read_environment_for_role(&env_path, role)?;
            install::validate_environment_for_role(&environment, role)?;

            install::required_file(&root.join("genesis.json"))?;
            install::validate_genesis(root, &environment["OUTBE_CHAIN_ID"])?;
            install::required_file(&root.join(".bb-crs/bn254_g1.dat"))?;
        }
    }

    if options.prepare_runtime_dirs {
        prepare_directories(root, role, options.apply_ownership)?;
    }
    Ok(())
}

fn verify_mount(root: &Path, mountinfo: &str) -> Result<()> {
    let target = root.to_string_lossy();
    let target = target.trim_end_matches('/');
    let mut matched_options: Option<&str> = None;

    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let mount_point = decode_mountinfo_path(fields[4]);
        if mount_point.trim_end_matches('/') == target {
            matched_options = Some(fields[5]);
        }
    }

    let raw_options =
        matched_options.ok_or_else(|| eyre::eyre!("{} is not a mount point", root.display()))?;
    let present: BTreeSet<&str> = raw_options.split(',').collect();
    let missing: Vec<&str> = REQUIRED_MOUNT_OPTIONS
        .iter()
        .copied()
        .filter(|option| !present.contains(option))
        .collect();
    ensure!(
        missing.is_empty(),
        "{} mount is missing required options: {} (got: {raw_options})",
        root.display(),
        missing.join(", "),
    );
    Ok(())
}

fn decode_mountinfo_path(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &encoded[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(octal, 8) {
                decoded.push(value);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn verify_tree(root: &Path) -> Result<()> {
    let root_fd = open_dir_nofollow(CWD, root)?;
    let skipped = root.join(SKIPPED_STATE_SUBTREE);

    // Check the skipped datadir itself is not a symlink or device node, then clean stale sockets.
    if let Ok(metadata) = fs::symlink_metadata(&skipped) {
        let ft = metadata.file_type();
        ensure!(
            ft.is_dir() && !ft.is_symlink(),
            "refusing non-directory service state root: {}",
            skipped.display()
        );
    }
    for relative in STALE_SOCKETS {
        remove_stale_socket(&root_fd, root, Path::new(relative))?;
    }

    let mut iter = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(entry) = iter.next() {
        let entry =
            entry.wrap_err_with(|| format!("failed to scan data tree under {}", root.display()))?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .wrap_err_with(|| format!("path escaped data root: {}", path.display()))?;
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("failed to stat {}", path.display()))?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            bail!("refusing symlink in data tree: {}", path.display());
        }
        if file_type.is_block_device() {
            bail!("refusing block device in data tree: {}", path.display());
        }
        if file_type.is_char_device() {
            bail!("refusing character device in data tree: {}", path.display());
        }

        if path == skipped {
            // Do not descend into the large reth datadir.
            iter.skip_current_dir();
            continue;
        }

        if is_service_state(relative) {
            continue;
        }

        // Static inputs must contain only non-executable regular files and directories.
        if file_type.is_dir() {
            continue;
        }
        if file_type.is_socket() {
            bail!("refusing socket in static data tree: {}", path.display());
        }
        if file_type.is_fifo() {
            bail!("refusing fifo in static data tree: {}", path.display());
        }
        if file_type.is_file() {
            let mode = metadata.mode() & 0o7777;
            ensure!(
                mode & 0o111 == 0,
                "refusing executable file on data disk (mode {mode:04o}): {}",
                path.display()
            );
            ensure!(
                mode & 0o6000 == 0,
                "refusing setuid/setgid file on data disk (mode {mode:04o}): {}",
                path.display()
            );
            continue;
        }
        bail!(
            "refusing unsupported file type in data tree: {}",
            path.display()
        );
    }
    Ok(())
}

fn is_service_state(relative: &Path) -> bool {
    SERVICE_STATE_ROOTS
        .iter()
        .any(|prefix| relative.starts_with(prefix))
}

fn verify_config_file(path: &Path, require_root_owner: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("missing required configuration: {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "configuration is not a regular file: {}",
        path.display()
    );
    if require_root_owner {
        ensure!(
            metadata.uid() == 0,
            "configuration must be root-owned: {}",
            path.display()
        );
    }
    let mode = metadata.mode() & 0o7777;
    ensure!(
        mode & 0o022 == 0,
        "configuration must not be group/world writable (mode {mode:04o}): {}",
        path.display()
    );
    Ok(())
}

fn verify_secrets(root: &Path, bin_dir: Option<&Path>) -> Result<()> {
    let keys = root.join("keys");
    let keys_meta = fs::symlink_metadata(&keys)
        .wrap_err_with(|| format!("keys directory is missing: {}", keys.display()))?;
    ensure!(
        keys_meta.is_dir() && !keys_meta.file_type().is_symlink(),
        "keys path is not a directory: {}",
        keys.display()
    );
    for relative in install::REQUIRED_SECRETS {
        let path = keys.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("required secret is missing: {}", path.display()))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "required secret is not a regular file: {}",
            path.display()
        );
        let mode = metadata.mode() & 0o7777;
        ensure!(
            mode & 0o077 == 0,
            "required secret must have owner-only permissions (mode {mode:04o}): {}",
            path.display()
        );
    }
    for relative in install::CANONICAL_HEX_KEYS {
        install::canonical_key(&keys.join(relative))?;
    }
    if let Some(bin_dir) = bin_dir {
        install::verify_signing_key(bin_dir, &keys)?;
    } else {
        let signing = fs::read_to_string(keys.join("signing-key.hex"))
            .wrap_err("failed to read signing-key.hex")?;
        let trimmed = signing.trim();
        ensure!(!trimmed.is_empty(), "signing-key.hex must not be empty");
    }
    Ok(())
}

fn require_outbe_account() -> Result<(u32, u32)> {
    ensure!(
        process::command_succeeds("getent", &["group", "outbe"])
            && process::command_succeeds("getent", &["passwd", "outbe"]),
        "required system account 'outbe' is missing from the VM image"
    );
    let uid = install::numeric_id("-u")?;
    let gid = install::numeric_id("-g")?;
    Ok((uid, gid))
}

fn prepare_directories(root: &Path, role: NodeRole, apply_ownership: bool) -> Result<()> {
    let ownership = if apply_ownership {
        Some(require_outbe_account()?)
    } else {
        None
    };
    let root_fd = open_dir_nofollow(CWD, root)?;
    let outbe_owner = ownership;
    let root_group_outbe = ownership.map(|(_, gid)| (0, gid));
    let root_owner = ownership.map(|_| (0, 0));

    for relative in VALIDATOR_DIRS {
        ensure_dir_nofollow(&root_fd, root, Path::new(relative), 0o750, outbe_owner)?;
    }
    ensure_dir_nofollow(&root_fd, root, Path::new("state"), 0o750, root_group_outbe)?;
    ensure_dir_nofollow(
        &root_fd,
        root,
        Path::new("state/operator"),
        0o700,
        root_owner,
    )?;

    if root.join(NODE_ROLE_FILE).exists() {
        normalize_file_nofollow(
            &root_fd,
            root,
            Path::new(NODE_ROLE_FILE),
            0o640,
            root_group_outbe,
        )?;
    }

    match role {
        NodeRole::Founder | NodeRole::JoiningValidator => {
            for relative in RADICLE_DIRS {
                ensure_dir_nofollow(&root_fd, root, Path::new(relative), 0o700, outbe_owner)?;
            }
            ensure_dir_nofollow(
                &root_fd,
                root,
                Path::new("state/feeder"),
                0o700,
                outbe_owner,
            )?;
            ensure_dir_nofollow(
                &root_fd,
                root,
                Path::new("state/hyperlane"),
                0o700,
                outbe_owner,
            )?;

            // Normalize static keys directories and secret files without descending into radicle state.
            for relative in ["keys", "keys/radicle", "keys/radicle/keys"] {
                ensure_dir_nofollow(&root_fd, root, Path::new(relative), 0o700, outbe_owner)?;
            }
            for relative in install::REQUIRED_SECRETS {
                normalize_file_nofollow(
                    &root_fd,
                    root,
                    &Path::new("keys").join(relative),
                    0o600,
                    outbe_owner,
                )?;
            }
            for name in CONFIG_FILES {
                normalize_file_nofollow(&root_fd, root, Path::new(name), 0o640, root_group_outbe)?;
            }
        }
        NodeRole::FullNode => {
            normalize_file_nofollow(
                &root_fd,
                root,
                Path::new("validator.env"),
                0o640,
                root_group_outbe,
            )?;
        }
    }
    Ok(())
}

fn open_dir_nofollow(parent: impl AsFd, path: &Path) -> Result<OwnedFd> {
    openat(
        parent,
        path,
        OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("refusing symlink or non-directory: {}", path.display()))
}

fn ensure_dir_nofollow(
    root_fd: &OwnedFd,
    root: &Path,
    relative: &Path,
    mode: u32,
    ownership: Option<(u32, u32)>,
) -> Result<()> {
    let mut current_fd = open_dir_nofollow(root_fd, Path::new("."))?;
    let mut current_path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("invalid relative directory path: {}", relative.display());
        };
        current_path.push(name);
        match mkdirat(&current_fd, name, Mode::from_raw_mode(mode)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => {
                return Err(io::Error::from(error)).wrap_err_with(|| {
                    format!("failed to create directory: {}", current_path.display())
                });
            }
        }
        current_fd = open_dir_nofollow(&current_fd, Path::new(name)).wrap_err_with(|| {
            format!(
                "refusing symlink or non-directory at {}",
                current_path.display()
            )
        })?;
    }
    apply_fd_attrs(&current_fd, &current_path, mode, ownership)
}

fn normalize_file_nofollow(
    root_fd: &OwnedFd,
    root: &Path,
    relative: &Path,
    mode: u32,
    ownership: Option<(u32, u32)>,
) -> Result<()> {
    let full_path = root.join(relative);
    let parent_rel = relative.parent().unwrap_or_else(|| Path::new(""));
    let file_name = relative
        .file_name()
        .ok_or_else(|| eyre::eyre!("invalid file path: {}", full_path.display()))?;
    let parent_fd = open_relative_dir_nofollow(root_fd, root, parent_rel)?;
    let fd = openat(
        &parent_fd,
        file_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| {
        format!(
            "refusing symlink or unreadable file: {}",
            full_path.display()
        )
    })?;
    let stat = fstat(&fd)
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to stat {}", full_path.display()))?;
    ensure!(
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
        "expected regular file: {}",
        full_path.display()
    );
    apply_fd_attrs(&fd, &full_path, mode, ownership)
}

fn open_relative_dir_nofollow(root_fd: &OwnedFd, root: &Path, relative: &Path) -> Result<OwnedFd> {
    let mut current_fd = open_dir_nofollow(root_fd, Path::new("."))?;
    let mut current_path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current_path.push(name);
        current_fd = open_dir_nofollow(&current_fd, Path::new(name)).wrap_err_with(|| {
            format!(
                "refusing symlink or non-directory at {}",
                current_path.display()
            )
        })?;
    }
    Ok(current_fd)
}

fn remove_stale_socket(root_fd: &OwnedFd, root: &Path, relative: &Path) -> Result<()> {
    let full_path = root.join(relative);
    let parent_rel = relative.parent().unwrap_or_else(|| Path::new(""));
    let file_name = relative
        .file_name()
        .ok_or_else(|| eyre::eyre!("invalid socket path: {}", full_path.display()))?;
    let Ok(parent_fd) = open_relative_dir_nofollow(root_fd, root, parent_rel) else {
        return Ok(());
    };
    let stat = match statat(&parent_fd, file_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(io::Error::from(error))
                .wrap_err_with(|| format!("failed to stat {}", full_path.display()));
        }
    };
    if FileType::from_raw_mode(stat.st_mode) == FileType::Socket {
        unlinkat(&parent_fd, file_name, AtFlags::empty())
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("failed to remove stale socket: {}", full_path.display()))?;
    }
    Ok(())
}

fn apply_fd_attrs(
    fd: &OwnedFd,
    path: &Path,
    mode: u32,
    ownership: Option<(u32, u32)>,
) -> Result<()> {
    fchmod(fd, Mode::from_raw_mode(mode))
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to chmod {}", path.display()))?;
    if let Some((uid, gid)) = ownership {
        fchown(fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("failed to chown {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::{
        fs::{symlink, PermissionsExt},
        net::UnixListener,
    };

    use super::*;

    fn mountinfo_for(root: &Path, options: &str) -> String {
        format!(
            "100 1 8:1 / {} {} - ext4 /dev/vda1 rw\n",
            root.display(),
            options
        )
    }

    fn write_file_mode(path: &Path, content: &[u8], mode: u32) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn build_fake_bin_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let keygen = dir.path().join("outbe-keygen");
        write_file_mode(&keygen, b"#!/bin/sh\nexit 0\n", 0o755);
        dir
    }

    fn valid_env() -> String {
        let peer_key = "a".repeat(96);
        format!(
            concat!(
                "OUTBE_EXTERNAL_IP=\"10.0.0.1\"\n",
                "OUTBE_RPC_BIND_IP=\"127.0.0.1\"\n",
                "OUTBE_CERTIFIED_RPC=\"http://127.0.0.1:8545\"\n",
                "OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n",
                "OUTBE_BOOTNODES=\"enode://abcd@10.0.0.1:30303\"\n",
                "OUTBE_CONSENSUS_PEERS=\"{peer_key}@10.0.0.1:30400\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"--consensus.use-local-defaults\"\n",
                "OUTBE_CHAIN_ID=\"707\"\n",
            ),
            peer_key = peer_key
        )
    }

    fn build_valid_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_file_mode(&root.join("validator.env"), valid_env().as_bytes(), 0o640);
        write_file_mode(
            &root.join("feeder-public.toml"),
            b"pairs = \"pairs.toml\"\n",
            0o640,
        );
        write_file_mode(
            &root.join("hyperlane-public.toml"),
            concat!(
                "[hyperlane]\n",
                "poll_interval_secs = 30\n",
                "gasless = true\n\n",
                "[health]\n",
                "enabled = true\n",
                "bind_address = \"127.0.0.1:9003\"\n\n",
                "[storage]\n",
                "db_path = \"/opt/outbe-chain/state/hyperlane/db\"\n\n",
                "[server]\n",
                "bind_address = \"0.0.0.0:9000\"\n",
                "public_url = \"http://10.0.0.1:9000\"\n\n",
                "[[origin]]\n",
                "domain = 707\n",
                "rpc_urls = [\"http://127.0.0.1:8545\"]\n",
                "rpc_consensus = \"single\"\n",
                "reorg_period = 0\n",
            )
            .as_bytes(),
            0o640,
        );
        write_file_mode(
            &root.join("genesis.json"),
            br#"{"config":{"chainId":707}}"#,
            0o644,
        );
        write_file_mode(&root.join(".bb-crs/bn254_g1.dat"), b"crs", 0o644);

        let key_hex = "a".repeat(64);
        for relative in install::REQUIRED_SECRETS {
            write_file_mode(&root.join("keys").join(relative), key_hex.as_bytes(), 0o600);
        }
        dir
    }

    fn test_options(bin_dir: &Path) -> BootOptions<'_> {
        BootOptions {
            bin_dir: Some(bin_dir),
            require_root_owner: false,
            apply_ownership: false,
            prepare_runtime_dirs: true,
            expected_role: None,
        }
    }

    #[test]
    fn valid_layout_passes_and_creates_state_dirs() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();
        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec,relatime");

        boot_root(root, &mountinfo, &test_options(bins.path())).unwrap();

        for relative in [
            "validator-0/data",
            "validator-0/consensus",
            "validator-0/logs",
            "keys/radicle/storage",
            "keys/radicle/node",
            "keys/radicle/cobs",
            "state/operator",
            "state/feeder",
            "state/hyperlane",
        ] {
            assert!(root.join(relative).is_dir(), "missing dir: {relative}");
        }
    }

    #[test]
    fn second_boot_with_service_state_and_stale_sockets_passes() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();

        // Populate stale sockets and runtime files left by a hard power-off.
        let radicle_sock = root.join("keys/radicle/node/outbe-control.sock");
        fs::create_dir_all(radicle_sock.parent().unwrap()).unwrap();
        let _radicle_listener = UnixListener::bind(&radicle_sock).unwrap();

        let reth_ipc = root.join("validator-0/data/reth.ipc");
        fs::create_dir_all(reth_ipc.parent().unwrap()).unwrap();
        let _reth_listener = UnixListener::bind(&reth_ipc).unwrap();

        let git_hook = root.join("keys/radicle/storage/repo/hooks/post-receive");
        write_file_mode(&git_hook, b"#!/bin/sh\n", 0o755);

        let reth_file = root.join("validator-0/data/db/mdbx.dat");
        write_file_mode(&reth_file, b"db", 0o755);

        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
        boot_root(root, &mountinfo, &test_options(bins.path())).unwrap();

        assert!(
            !radicle_sock.exists(),
            "stale radicle socket must be removed"
        );
        assert!(!reth_ipc.exists(), "stale reth.ipc socket must be removed");
        assert!(
            git_hook.is_file(),
            "radicle storage files must be preserved"
        );
        assert!(reth_file.is_file(), "reth datadir files must be preserved");
    }

    #[test]
    fn rejects_missing_mount_or_wrong_mount_flags() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();

        let err = boot_root(root, "", &test_options(bins.path())).unwrap_err();
        assert!(err.to_string().contains("is not a mount point"), "{err}");

        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,relatime");
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(err.to_string().contains("noexec"), "{err}");

        let mountinfo = mountinfo_for(root, "rw,noexec");
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(err.to_string().contains("nosuid"), "{err}");
        assert!(err.to_string().contains("nodev"), "{err}");
    }

    #[test]
    fn rejects_symlink_in_data_tree() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();
        let link = root.join("evil-link");
        symlink("/etc/passwd", &link).unwrap();

        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(err.to_string().contains("refusing symlink"), "{err}");
        assert!(
            err.to_string().contains(&link.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn normalization_refuses_planted_symlinks_without_touching_outside_targets() {
        let dir = build_valid_root();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();

        let outside_dir = outside.path().join("target-dir");
        fs::create_dir(&outside_dir).unwrap();
        fs::set_permissions(&outside_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let outside_file = outside.path().join("target-file");
        write_file_mode(&outside_file, b"untouched", 0o644);

        // Plant a symlink where a state directory is expected to be normalized.
        fs::create_dir_all(root.join("state")).unwrap();
        symlink(&outside_dir, root.join("state/feeder")).unwrap();
        let err = prepare_directories(root, NodeRole::Founder, false).unwrap_err();
        assert!(
            err.to_string().contains("refusing symlink"),
            "expected symlink refusal, got: {err}"
        );
        assert_eq!(
            fs::metadata(&outside_dir).unwrap().mode() & 0o777,
            0o755,
            "outside directory mode must not be modified"
        );

        // Replace directory symlink with a real dir and plant a file symlink on a config file.
        fs::remove_file(root.join("state/feeder")).unwrap();
        fs::remove_file(root.join("feeder-public.toml")).unwrap();
        symlink(&outside_file, root.join("feeder-public.toml")).unwrap();
        let err = prepare_directories(root, NodeRole::Founder, false).unwrap_err();
        assert!(
            err.to_string().contains("refusing symlink"),
            "expected symlink refusal, got: {err}"
        );
        assert_eq!(
            fs::metadata(&outside_file).unwrap().mode() & 0o777,
            0o644,
            "outside file mode must not be modified"
        );
    }

    #[test]
    fn rejects_executable_file_in_static_data_tree() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();
        let exec_path = root.join("rogue-script");
        write_file_mode(&exec_path, b"#!/bin/sh\n", 0o755);

        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(
            err.to_string().contains("refusing executable file"),
            "{err}"
        );
        assert!(
            err.to_string().contains(&exec_path.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn rejects_unexpected_socket_in_static_data_tree() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();
        let sock = root.join("stray.sock");
        let _listener = UnixListener::bind(&sock).unwrap();

        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(err.to_string().contains("refusing socket"), "{err}");
        assert!(
            err.to_string().contains(&sock.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn rejects_missing_key_and_loose_key_permissions() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();
        let key_path = root.join("keys/evm-key.hex");
        fs::remove_file(&key_path).unwrap();

        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(
            err.to_string().contains("required secret is missing"),
            "{err}"
        );
        assert!(
            err.to_string().contains(&key_path.display().to_string()),
            "{err}"
        );

        let key_hex = "a".repeat(64);
        write_file_mode(&key_path, key_hex.as_bytes(), 0o644);
        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        assert!(err.to_string().contains("owner-only permissions"), "{err}");
        assert!(
            err.to_string().contains(&key_path.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn rejects_unknown_validator_env_keys() {
        let dir = build_valid_root();
        let bins = build_fake_bin_dir();
        let root = dir.path();
        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");

        for bad_line in [
            "BB_CRS_PATH=\"/tmp/evil.dat\"\n",
            "LD_PRELOAD=\"/tmp/evil.so\"\n",
        ] {
            let mut env = valid_env();
            env.push_str(bad_line);
            write_file_mode(&root.join("validator.env"), env.as_bytes(), 0o640);
            let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
            assert!(
                err.to_string().contains("unknown validator.env key"),
                "{err}"
            );
        }
    }

    #[test]
    fn rejects_invalid_signing_key_via_outbe_keygen() {
        let dir = build_valid_root();
        let bins = tempfile::tempdir().unwrap();
        write_file_mode(
            &bins.path().join("outbe-keygen"),
            concat!(
                "#!/bin/sh\n",
                "echo 'STDOUT_SECRET_MARKER_deadbeef'\n",
                "echo 'STDERR_SECRET_MARKER_cafebabe' >&2\n",
                "exit 1\n",
            )
            .as_bytes(),
            0o755,
        );
        let root = dir.path();
        let mountinfo = mountinfo_for(root, "rw,nosuid,nodev,noexec");

        let err = boot_root(root, &mountinfo, &test_options(bins.path())).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("signing-key.hex failed outbe-keygen verify"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("STDOUT_SECRET_MARKER_deadbeef"),
            "verify_signing_key must never leak stdout: {rendered}"
        );
        assert!(
            !rendered.contains("STDERR_SECRET_MARKER_cafebabe"),
            "verify_signing_key must never leak stderr: {rendered}"
        );
    }

    fn build_valid_full_node_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_file_mode(&root.join(NODE_ROLE_FILE), b"full-node\n", 0o640);
        write_file_mode(
            &root.join("validator.env"),
            concat!(
                "OUTBE_EXTERNAL_IP=\"10.0.0.2\"\n",
                "OUTBE_RPC_BIND_IP=\"127.0.0.1\"\n",
                "OUTBE_CERTIFIED_RPC=\"http://10.0.0.1:8545\"\n",
                "OUTBE_BOOTNODES=\"enode://abcd@10.0.0.1:30303\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"--consensus.use-local-defaults\"\n",
                "OUTBE_CHAIN_ID=\"707\"\n",
            )
            .as_bytes(),
            0o640,
        );
        write_file_mode(
            &root.join("genesis.json"),
            br#"{"config":{"chainId":707}}"#,
            0o644,
        );
        write_file_mode(&root.join(".bb-crs/bn254_g1.dat"), b"crs", 0o644);
        dir
    }

    #[test]
    fn role_marker_validates_joining_and_full_node_and_forbids_full_node_secrets() {
        let bins = build_fake_bin_dir();

        // 1. Joining validator marker passes full validator boot checks.
        let joining = build_valid_root();
        write_file_mode(
            &joining.path().join(NODE_ROLE_FILE),
            b"joining-validator\n",
            0o640,
        );
        let mountinfo = mountinfo_for(joining.path(), "rw,nosuid,nodev,noexec");
        assert_eq!(
            read_role_marker(joining.path(), false).unwrap(),
            NodeRole::JoiningValidator
        );
        boot_root(joining.path(), &mountinfo, &test_options(bins.path())).unwrap();
        assert!(joining.path().join("state/hyperlane").is_dir());

        // 2. Full node passes with zero keys and creates only validator-0 + state/operator.
        let full = build_valid_full_node_root();
        let full_mount = mountinfo_for(full.path(), "rw,nosuid,nodev,noexec");
        verify_static_tree_for_role(full.path(), NodeRole::FullNode, None).unwrap();
        assert!(
            !full.path().join("validator-0").exists(),
            "static verification must not create runtime directories"
        );
        boot_root(full.path(), &full_mount, &test_options(bins.path())).unwrap();
        assert!(full.path().join("validator-0/data").is_dir());
        assert!(full.path().join("state/operator").is_dir());
        assert!(!full.path().join("state/feeder").exists());
        assert!(!full.path().join("state/hyperlane").exists());
        assert!(!full.path().join("keys").exists());

        // 3. Full node rejects validator keys, feeder/hyperlane configs, and validator env keys.
        let bad_keys = build_valid_full_node_root();
        write_file_mode(
            &bad_keys.path().join("keys/evm-key.hex"),
            "a".repeat(64).as_bytes(),
            0o600,
        );
        let err =
            verify_static_tree_for_role(bad_keys.path(), NodeRole::FullNode, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("must not contain validator artifact"),
            "{err}"
        );

        let bad_feeder = build_valid_full_node_root();
        write_file_mode(
            &bad_feeder.path().join("feeder-public.toml"),
            b"[oracle]\n",
            0o640,
        );
        let err =
            verify_static_tree_for_role(bad_feeder.path(), NodeRole::FullNode, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("must not contain validator artifact"),
            "{err}"
        );

        let bad_env = build_valid_full_node_root();
        let mut env = fs::read_to_string(bad_env.path().join("validator.env")).unwrap();
        env.push_str("OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n");
        write_file_mode(&bad_env.path().join("validator.env"), env.as_bytes(), 0o640);
        let err =
            verify_static_tree_for_role(bad_env.path(), NodeRole::FullNode, None).unwrap_err();
        assert!(
            err.to_string().contains("unknown validator.env key"),
            "{err}"
        );

        // 4. Role mismatch and corrupt/loose role markers fail closed.
        let err = verify_static_tree_for_role(full.path(), NodeRole::Founder, None).unwrap_err();
        assert!(err.to_string().contains("role mismatch"), "{err}");

        let bad_marker = build_valid_full_node_root();
        write_file_mode(&bad_marker.path().join(NODE_ROLE_FILE), b"bogus\n", 0o640);
        assert!(verify_static_tree_for_role(bad_marker.path(), NodeRole::FullNode, None).is_err());

        write_file_mode(
            &bad_marker.path().join(NODE_ROLE_FILE),
            b"full-node\n",
            0o666,
        );
        assert!(verify_static_tree_for_role(bad_marker.path(), NodeRole::FullNode, None).is_err());
    }
}
