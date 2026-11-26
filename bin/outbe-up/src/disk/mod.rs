//! Local single-VM data disk preparation and staging cleanup (`outbe-up disk prepare|clean`).
//!
//! ## Signal handling and staging cleanup
//!
//! `SIGINT` and `SIGTERM` flip a cooperative cancellation flag that terminates child process
//! groups (`outbe-keygen`, `gzip`, `sfdisk`, `mke2fs`, `debugfs`) and unwinds all staging
//! `TempDir` guards (`.outbe-prepare-*`, `.outbe-disk-*`, `.outbe-vm-disk-*`) before exiting
//! non-zero without publishing the target image.
//!
//! An uncatchable `SIGKILL` (or sudden host power loss) cannot run in-process cleanup; pass
//! `--cleanup-stale` to remove leftover current-UID staging directories in the output directory.

mod clean;
mod prepare;

use std::path::Path;

use eyre::Result;
use outbe_operator_vm::local::{default_state_dir, expand_tilde, LocalVmStore};

pub use clean::{clean, CleanArgs};
pub use prepare::{prepare, PrepareArgs};

pub(super) fn open_store(state_dir: Option<&Path>, fix_permissions: bool) -> Result<LocalVmStore> {
    let dir = match state_dir {
        Some(path) => expand_tilde(path)?,
        None => default_state_dir()?,
    };
    Ok(LocalVmStore::new(dir).with_fix_permissions(fix_permissions))
}

pub(super) fn plural_directory(count: usize) -> &'static str {
    if count == 1 {
        "y"
    } else {
        "ies"
    }
}
