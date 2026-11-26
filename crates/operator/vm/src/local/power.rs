//! Start and stop of locally recorded VMs.

use std::time::{Duration, Instant};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    state::{validate_expected_vm_id, verify_expected_vm_id},
    store::VmJournal,
    vmm_ops::{request_stop, require_owned_vmm_info, stop_reached, wait_until_stopped, Poll},
    LocalVmPhase, LocalVmStore, FORCE_STOP_DKG_WARNING,
};
use crate::vmm::{VmInfo, VmmClient, VmmTransport};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub already_running: bool,
    pub vmm_status: String,
}

/// Options controlling `stop_local_vm` behavior.
#[derive(Clone, Copy, Debug)]
pub struct StopLocalVmOptions {
    /// Send forced `StopVm` instead of graceful `ShutdownVm`.
    pub force: bool,
    /// Total budget for lock acquisition, the initial `GetInfo`, the stop RPC, and all polling.
    pub timeout: Duration,
    pub poll_interval: Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StopLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub already_stopped: bool,
    pub forced: bool,
    /// True when `--force` sent `StopVm` on an already-`exited` VM to clear dstack's `.started`
    /// flag and prevent automatic restart.
    #[serde(default)]
    pub cleared_started_flag: bool,
    pub vmm_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// How one stop request treats the VM, which selects its RPC and journal steps.
#[derive(Clone, Copy, Eq, PartialEq)]
enum StopMode {
    /// `ShutdownVm`.
    Graceful,
    /// `StopVm` on a VM that has not exited.
    Force,
    /// `StopVm` on an `exited` VM to clear dstack's `.started` flag.
    ClearStartedFlag,
}

impl StopMode {
    fn select(force: bool, info: &VmInfo) -> Self {
        if !force {
            return Self::Graceful;
        }
        if !info.running && info.status == VmInfo::STATUS_EXITED {
            return Self::ClearStartedFlag;
        }
        Self::Force
    }

    fn begin_step(self) -> &'static str {
        match self {
            Self::Graceful => "shutting-down",
            Self::Force => "force-stopping",
            Self::ClearStartedFlag => "clearing-started-flag",
        }
    }

    fn done_step(self) -> &'static str {
        match self {
            Self::Graceful => "stopped",
            Self::Force => "force-stopped",
            Self::ClearStartedFlag => "cleared-started-flag",
        }
    }
}

/// Start one locally recorded VM.
pub fn start_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
) -> Result<StartLocalVmOutput> {
    start_local_vm_with_expected_id(store, vmm, name, None)
}

/// Start one locally recorded VM, verifying an optional `--expect-vm-id` under the per-VM lock
/// before issuing any VMM RPC.
pub fn start_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
) -> Result<StartLocalVmOutput> {
    validate_expected_vm_id(expect_vm_id)?;
    let guard = store.lock_vm(name)?;
    let journal = VmJournal {
        store,
        guard: &guard,
    };
    let mut state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    ensure!(
        state.phase == LocalVmPhase::Ready,
        "VM `{name}` ({}) is in phase `{:?}`, not `Ready`; resume `outbe-up vm create` or clean up with `outbe-up vm remove --name {name}`",
        state.vm_id,
        state.phase
    );

    let info = require_owned_vmm_info(vmm, &state, None)?;
    if info.running {
        return Ok(StartLocalVmOutput {
            name: state.name,
            vm_id: state.vm_id,
            already_running: true,
            vmm_status: info.status,
        });
    }

    journal.record(&mut state, "starting", None)?;

    vmm.start_vm(&state.vm_id)
        .wrap_err_with(|| format!("start VM `{name}` ({})", state.vm_id))?;

    let post_status = vmm
        .get_info(&state.vm_id)
        .ok()
        .and_then(|resp| resp.info)
        .map(|i| i.status)
        .unwrap_or_else(|| VmInfo::STATUS_RUNNING.to_owned());

    journal.record(&mut state, "started", Some(format!("status={post_status}")))?;

    Ok(StartLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        already_running: false,
        vmm_status: post_status,
    })
}

/// Stop one locally recorded VM gracefully (`ShutdownVm`), or forcefully (`StopVm`) only when
/// `options.force == true`.
///
/// The total `options.timeout` bounds the entire operation, including per-VM lock acquisition,
/// initial `GetInfo`, the stop RPC, and all polling calls. When `force == true` and the VM is
/// already `exited` (guest powered off while dstack's `.started` flag remains set), `StopVm` is
/// still sent so dstack clears `.started`, disables automatic restart, and transitions the VM to
/// `stopped`. Never falls back from graceful shutdown to forced stop.
pub fn stop_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    options: StopLocalVmOptions,
) -> Result<StopLocalVmOutput> {
    stop_local_vm_with_expected_id(store, vmm, name, None, options)
}

/// Same as [`stop_local_vm`], verifying an optional `--expect-vm-id` under the per-VM lock before
/// issuing any VMM RPC.
pub fn stop_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
    options: StopLocalVmOptions,
) -> Result<StopLocalVmOutput> {
    validate_expected_vm_id(expect_vm_id)?;
    let force = options.force;
    let poll = Poll {
        deadline: Instant::now() + options.timeout,
        interval: options.poll_interval,
    };
    let guard = store.lock_vm_with_options(name, Some(poll.deadline), None)?;
    let journal = VmJournal {
        store,
        guard: &guard,
    };
    let mut state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    ensure!(
        state.phase != LocalVmPhase::Creating,
        "VM `{name}` was interrupted during creation (no vm_id recorded); resume `outbe-up vm create` or clean up with `outbe-up vm remove --name {name}`"
    );
    let info = require_owned_vmm_info(vmm, &state, Some(poll.deadline))?;

    if stop_reached(&info, force) {
        return Ok(StopLocalVmOutput {
            name: state.name,
            vm_id: state.vm_id,
            already_stopped: true,
            forced: false,
            cleared_started_flag: false,
            vmm_status: info.status,
            warning: None,
        });
    }

    let mode = StopMode::select(force, &info);
    journal.record(&mut state, mode.begin_step(), None)?;
    request_stop(vmm, &state, force, poll.deadline)?;

    let final_status = wait_until_stopped(vmm, &state, poll, force)?;
    journal.record(
        &mut state,
        mode.done_step(),
        Some(format!("status={final_status}")),
    )?;

    Ok(StopLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        already_stopped: false,
        forced: force,
        cleared_started_flag: mode == StopMode::ClearStartedFlag,
        vmm_status: final_status,
        warning: force.then(|| FORCE_STOP_DKG_WARNING.to_owned()),
    })
}
