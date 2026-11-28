//! Single-VM local lifecycle management over the local dstack VMM Unix socket.
//!
//! Stores one versioned state file per VM under `${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up/vms/`
//! (`0700` dirs, `0600` files), protected by an `O_CLOEXEC` per-VM exclusive `flock` and atomic
//! fsync+rename journaling before every fallible step (including a durable `Creating` intent with a
//! client-chosen VMM annotation token written before `CreateVm`). Only VMs recorded in our state
//! store are ever started, stopped, inspected, or removed.

mod create;
mod lock;
mod paths;
mod power;
mod private_fs;
mod recovery;
mod remove;
mod staging;
mod state;
mod status;
mod store;
#[cfg(test)]
mod tests;
mod update;
mod vmm_ops;

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use eyre::{bail, Result};

pub use self::{
    create::{
        create_local_vm, create_local_vm_cancellable, create_local_vm_with_disk_stager,
        create_local_vm_with_disk_stager_cancellable, CreateLocalVmOptions, CreateLocalVmOutput,
    },
    lock::{StagingLockGuard, VmLockGuard},
    paths::{
        default_state_dir, default_vmm_home, default_vmm_socket, expand_tilde, resolve_vmm_paths,
    },
    power::{
        start_local_vm, start_local_vm_with_expected_id, stop_local_vm,
        stop_local_vm_with_expected_id, StartLocalVmOutput, StopLocalVmOptions, StopLocalVmOutput,
    },
    remove::{remove_local_vm, RemoveLocalVmOptions, RemoveLocalVmOutput},
    staging::{CleanStagingOutput, StagedDiskInfo},
    state::{
        validate_flavor, validate_owner_tag, validate_vm_name, JournalEntry, LocalVmPhase,
        LocalVmState,
    },
    status::{
        logs_local_vm, logs_local_vm_with_expected_id, status_all_local_vms, status_local_vm,
        status_local_vm_with_expected_id, LocalVmLogsOutput, LocalVmStatusOutput,
    },
    update::{
        update_local_vm, update_local_vm_cancellable, UpdateLocalVmOptions, UpdateLocalVmOutput,
    },
};

pub const LOCAL_VM_STATE_VERSION: u32 = 1;
pub const DEFAULT_LOCAL_VCPU: u32 = 8;
pub const DEFAULT_LOCAL_MEMORY_GIB: u32 = 16;
pub const DEFAULT_LOCAL_DISK_GIB: u32 = 100;
pub const DEFAULT_LOCAL_FLAVOR: &str = "dev";
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Default grace window when polling `Status` for an in-flight `Creating` VM before refusing.
pub const DEFAULT_CREATE_RECOVERY_GRACE: Duration = Duration::from_secs(30);

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// VMM manifest annotation key carrying our unique client-chosen recovery marker.
///
/// Security note: this token is a bearer marker stored in the VMM manifest and readable via
/// `GetInfo`/`Status` by any client authorized to access the VMM Unix socket — it carries the
/// same trust level as the VMM socket itself, not cryptographic proof of origin against another
/// VMM-authorized peer. Adoption therefore also verifies `name`, `image`, `vcpu`, `memory`, and
/// `app_id` against our local journal.
pub const CREATE_TOKEN_ANNOTATION: &str = "outbe-up.create-token";

/// VMM manifest annotation key carrying an optional caller-assigned ownership tag
/// (e.g. `outbe-net:<network>:<genesis8>:<member>`).
pub const OWNER_TAG_ANNOTATION: &str = "outbe-up.owner-tag";

/// Warning displayed when `--force` bypasses graceful guest shutdown (`ShutdownVm`).
pub const FORCE_STOP_DKG_WARNING: &str = "WARNING: forced VM stop (--force) bypasses graceful guest shutdown and can lose a pending DKG boundary snapshot (see /home/ubuntu/tdx/12-defect-pending-dkg-snapshot.md).";

/// Per-VM state store rooted at `${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up`.
#[derive(Clone, Debug)]
pub struct LocalVmStore {
    root: PathBuf,
    fix_permissions: bool,
}

fn check_canceled(cancel: Option<&AtomicBool>, step: &str) -> Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        bail!("operation interrupted by signal while {step}");
    }
    Ok(())
}
