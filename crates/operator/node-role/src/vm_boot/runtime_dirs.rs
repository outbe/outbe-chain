//! Runtime directory layout, permissions, and ownership on the VM data disk.

use std::path::Path;

use eyre::{ensure, Result};
use rustix::fs::CWD;

use super::{
    fd_ops::{ensure_dir_nofollow, normalize_file_nofollow, open_dir_nofollow},
    CONFIG_FILES,
};
use crate::{
    config::{NodeRole, NODE_ROLE_FILE},
    install, process,
};

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

pub(super) fn prepare_directories(
    root: &Path,
    role: NodeRole,
    apply_ownership: bool,
) -> Result<()> {
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
            normalize_file_nofollow(
                &root_fd,
                root,
                Path::new("reth.toml"),
                0o640,
                root_group_outbe,
            )?;
        }
    }
    Ok(())
}
