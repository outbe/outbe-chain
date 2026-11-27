//! VMM RPC steps shared by the lifecycle commands: ownership checks, stop requests, and bounded
//! polling until the VM reaches a target state.

use std::{
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, eyre, Context, Report, Result};

use super::{check_canceled, LocalVmState};
use crate::vmm::{GetInfoResponse, VmInfo, VmmClient, VmmTransport};

/// Deadline and interval for one polling loop.
#[derive(Clone, Copy, Debug)]
pub(super) struct Poll {
    pub(super) deadline: Instant,
    pub(super) interval: Duration,
}

impl Poll {
    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// Sleep one interval, clipped to the time left before the deadline.
    pub(super) fn sleep(&self) {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(self.interval));
    }
}

/// Return the VM info of a `GetInfo` response, or `None` when the VMM reports the VM gone.
pub(super) fn live_info(resp: GetInfoResponse) -> Option<VmInfo> {
    if resp.is_gone() {
        return None;
    }
    resp.info
}

/// Fetch `GetInfo` for a journaled VM and check that it still exists under the recorded name.
pub(super) fn require_owned_vmm_info<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    deadline: Option<Instant>,
) -> Result<VmInfo> {
    let resp = vmm.get_info_with_deadline(&state.vm_id, deadline)?;
    ensure!(
        !resp.is_gone(),
        "VM `{}` ({}) was not found on local VMM",
        state.name,
        state.vm_id
    );
    let info = resp.info.unwrap();
    ensure!(
        info.name == state.name,
        "VMM VM {} has unexpected name `{}` (expected `{}`)",
        state.vm_id,
        info.name,
        state.name
    );
    Ok(info)
}

/// True when the VM reached the stop state that the stop mode waits for: administratively
/// stopped for a forced stop, or stopped for a graceful shutdown.
pub(super) fn stop_reached(info: &VmInfo, force: bool) -> bool {
    if force {
        info.is_administratively_stopped()
    } else {
        info.is_stopped()
    }
}

/// Send `StopVm` when `force`, else `ShutdownVm`. Never falls back from one to the other.
pub(super) fn request_stop<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    force: bool,
    deadline: Instant,
) -> Result<()> {
    let name = &state.name;
    if force {
        vmm.stop_vm_with_deadline(&state.vm_id, Some(deadline))
            .wrap_err_with(|| format!("force-stop VM `{name}` ({})", state.vm_id))
    } else {
        vmm.shutdown_vm_with_deadline(&state.vm_id, Some(deadline))
            .wrap_err_with(|| format!("gracefully shut down VM `{name}` ({})", state.vm_id))
    }
}

pub(super) fn wait_until_stopped<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    poll: Poll,
    force: bool,
) -> Result<String> {
    loop {
        let resp = vmm.get_info_with_deadline(&state.vm_id, Some(poll.deadline))?;
        ensure!(
            !resp.is_gone(),
            "VM `{}` ({}) disappeared from local VMM while waiting for stop",
            state.name,
            state.vm_id
        );
        let info = resp.info.unwrap();
        if stop_reached(&info, force) {
            return Ok(info.status);
        }
        if poll.expired() {
            return Err(stop_timeout_error(state, &info, force));
        }
        poll.sleep();
    }
}

fn stop_timeout_error(state: &LocalVmState, info: &VmInfo, force: bool) -> Report {
    if force {
        return eyre!(
            "VM `{}` ({}) did not reach stopped state before timeout (last status: {}, running: {})",
            state.name,
            state.vm_id,
            info.status,
            info.running
        );
    }
    eyre!(
        "VM `{}` ({}) did not finish graceful shutdown before timeout (last status: {}, running: {}); did not force-stop (use --force only if you accept the pending-DKG snapshot risk in /home/ubuntu/tdx/12-defect-pending-dkg-snapshot.md)",
        state.name,
        state.vm_id,
        info.status,
        info.running
    )
}

pub(super) fn wait_until_running<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    poll: Poll,
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    loop {
        check_canceled(cancel, "waiting for VM to reach running state")?;
        let info = require_owned_vmm_info(vmm, state, Some(poll.deadline))?;
        if info.running && info.status == VmInfo::STATUS_RUNNING {
            return Ok(info.status);
        }
        if poll.expired() {
            bail!(
                "VM `{}` ({}) did not reach `running` state after StartVm (last status={}, running={})",
                state.name,
                state.vm_id,
                info.status,
                info.running
            );
        }
        poll.sleep();
    }
}

pub(super) fn wait_until_stopped_or_gone<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    poll: Poll,
    force: bool,
) -> Result<()> {
    loop {
        let resp = vmm.get_info_with_deadline(&state.vm_id, Some(poll.deadline))?;
        let Some(info) = live_info(resp) else {
            return Ok(());
        };
        if stop_reached(&info, force) {
            return Ok(());
        }
        if poll.expired() {
            return Err(removal_stop_timeout_error(state, &info, force));
        }
        poll.sleep();
    }
}

fn removal_stop_timeout_error(state: &LocalVmState, info: &VmInfo, force: bool) -> Report {
    if force {
        return eyre!(
            "VM `{}` ({}) did not stop before removal timeout (keeping state file for retry; last status: {})",
            state.name,
            state.vm_id,
            info.status
        );
    }
    eyre!(
        "VM `{}` ({}) did not finish graceful shutdown before removal timeout (keeping state file for retry; last status: {}); re-run with --force only if you accept the pending-DKG snapshot risk",
        state.name,
        state.vm_id,
        info.status
    )
}

pub(super) fn wait_until_gone<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    poll: Poll,
) -> Result<()> {
    loop {
        let resp = vmm.get_info_with_deadline(&state.vm_id, Some(poll.deadline))?;
        if resp.is_gone() {
            return Ok(());
        }
        let status = resp
            .info
            .map(|i| i.status)
            .unwrap_or_else(|| "unknown".to_owned());
        if poll.expired() {
            bail!(
                "VM `{}` ({}) did not finish asynchronous VMM removal before timeout (keeping state file for retry; last status: {status})",
                state.name,
                state.vm_id
            );
        }
        poll.sleep();
    }
}
