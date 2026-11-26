//! `vm remove`: stop, remove from the VMM, then delete the local state file.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    recovery::{compute_grace_deadline, probe_creating_intent},
    state::{validate_expected_vm_id, verify_expected_vm_id},
    store::VmJournal,
    vmm_ops::{live_info, request_stop, wait_until_gone, wait_until_stopped_or_gone, Poll},
    LocalVmPhase, LocalVmState, LocalVmStore, DEFAULT_CREATE_RECOVERY_GRACE, DEFAULT_POLL_INTERVAL,
    FORCE_STOP_DKG_WARNING,
};
use crate::vmm::{VmInfo, VmmClient, VmmTransport};

/// Options controlling `remove_local_vm` behavior.
#[derive(Clone, Debug)]
pub struct RemoveLocalVmOptions {
    pub force: bool,
    /// Allow deleting an unresolved `Creating` intent when no token-matched VM appears within the
    /// grace polling window.
    pub forget_intent: bool,
    /// Optional expected VM UUID verified under the per-VM exclusive lock before any VMM RPC.
    pub expect_vm_id: Option<String>,
    pub timeout: Duration,
    pub recovery_grace: Duration,
    pub poll_interval: Duration,
}

impl Default for RemoveLocalVmOptions {
    fn default() -> Self {
        Self {
            force: false,
            forget_intent: false,
            expect_vm_id: None,
            timeout: Duration::from_secs(120),
            recovery_grace: DEFAULT_CREATE_RECOVERY_GRACE,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoveLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub state_file: PathBuf,
    pub forced: bool,
    #[serde(default)]
    pub forgot_intent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Remove one locally recorded VM, waiting for asynchronous VMM removal before deleting the
/// state file. Guest OS images are never deleted.
///
/// If the VM is in `Creating` phase (interrupted before `CreateVm` returned an ID), polls
/// filtered `Status` + `GetInfo` during the grace window and verifies both
/// `CREATE_TOKEN_ANNOTATION` and journaled config before removing; if still absent, refuses to
/// delete the intent unless `options.forget_intent == true`. The total `options.timeout` bounds
/// lock acquisition, recovery grace polling, and all RPCs.
pub fn remove_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    options: RemoveLocalVmOptions,
) -> Result<RemoveLocalVmOutput> {
    validate_expected_vm_id(options.expect_vm_id.as_deref())?;
    let deadline = Instant::now() + options.timeout;
    let guard = store.lock_vm_with_options(name, Some(deadline), None)?;
    let journal = VmJournal {
        store,
        guard: &guard,
    };
    let mut state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, options.expect_vm_id.as_deref())?;

    // Recover an interrupted `Creating` record strictly by our unique annotation token + config.
    if state.is_unresolved_creating() {
        let poll = Poll {
            deadline: compute_grace_deadline(options.recovery_grace, Some(deadline)),
            interval: options.poll_interval,
        };
        let probe = probe_creating_intent(vmm, &state, poll, None)?;
        let Some(recovered) = probe.adopted else {
            return forget_creating_intent(journal, state, &probe.token, &options);
        };
        state.vm_id = recovered.id;
        state.phase = LocalVmPhase::Removing;
        journal.record(&mut state, "recovered-creating-for-remove", None)?;
    } else {
        state.phase = LocalVmPhase::Removing;
        journal.record(&mut state, "removing", None)?;
    }

    let poll = Poll {
        deadline,
        interval: options.poll_interval,
    };
    let warning = remove_from_vmm(vmm, journal, &mut state, options.force, poll)?;

    let state_file = store.delete_locked(&guard, name)?;
    Ok(RemoveLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        state_file,
        forced: options.force && warning.is_some(),
        forgot_intent: false,
        warning,
    })
}

/// Delete an unresolved `Creating` intent whose VM never appeared, only with `--forget-intent`.
fn forget_creating_intent(
    journal: VmJournal<'_>,
    state: LocalVmState,
    token: &str,
    options: &RemoveLocalVmOptions,
) -> Result<RemoveLocalVmOutput> {
    let name = &state.name;
    ensure!(
        options.forget_intent,
        "VM `{name}` has an unresolved `Creating` intent (token {token}) and did not appear on the local VMM within {}s; refusing to delete the intent because an earlier CreateVm may still complete. Re-run with `--forget-intent` once you have confirmed on the VMM that no VM was created",
        options.recovery_grace.as_secs()
    );
    let state_file = journal.store.delete_locked(journal.guard, name)?;
    Ok(RemoveLocalVmOutput {
        name: state.name,
        vm_id: String::new(),
        state_file,
        forced: false,
        forgot_intent: true,
        warning: None,
    })
}

/// Stop the VM if it runs, request VMM removal, and wait until the VMM no longer reports it.
/// Return the force-stop warning when a forced stop was sent.
fn remove_from_vmm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    journal: VmJournal<'_>,
    state: &mut LocalVmState,
    force: bool,
    poll: Poll,
) -> Result<Option<String>> {
    let resp = vmm.get_info_with_deadline(&state.vm_id, Some(poll.deadline))?;
    let Some(info) = live_info(resp) else {
        return Ok(None);
    };
    ensure!(
        info.name == state.name,
        "refusing to remove VM {}: VMM reports name `{}`, expected `{}`",
        state.vm_id,
        info.name,
        state.name
    );

    let mut warning = None;
    if info.running && info.status != VmInfo::STATUS_REMOVING {
        warning = force.then(|| FORCE_STOP_DKG_WARNING.to_owned());
        let step = if force {
            "force-stopping-for-remove"
        } else {
            "shutting-down-for-remove"
        };
        journal.record(state, step, None)?;
        request_stop(vmm, state, force, poll.deadline)?;
        wait_until_stopped_or_gone(vmm, state, poll, force)?;
    }

    request_vmm_removal(vmm, state, poll)?;
    Ok(warning)
}

/// Send `RemoveVm` unless removal is already in progress, then wait until the VM is gone.
fn request_vmm_removal<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    poll: Poll,
) -> Result<()> {
    let post_stop = vmm.get_info_with_deadline(&state.vm_id, Some(poll.deadline))?;
    if post_stop.is_gone() {
        return Ok(());
    }
    let status = post_stop
        .info
        .as_ref()
        .map(|i| i.status.as_str())
        .unwrap_or("");
    if status != VmInfo::STATUS_REMOVING {
        vmm.remove_vm_with_deadline(&state.vm_id, Some(poll.deadline))
            .wrap_err_with(|| format!("remove VM `{}` ({})", state.name, state.vm_id))?;
    }
    wait_until_gone(vmm, state, poll)
}
